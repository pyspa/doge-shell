use crate::dirs;
use crate::parser::{self, Rule};
use crate::process::{self, Job, JobProcess, Redirect, SubshellType};
use crate::shell::Shell;
use anyhow::{Context as _, Result, bail};
use dsh_types::Context;
use nix::libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::termios::tcgetattr;
use nix::unistd::pipe;
use pest::iterators::Pair;
use std::os::fd::{AsRawFd, BorrowedFd, IntoRawFd, RawFd};
use tracing::{debug, warn};

#[derive(Debug)]
pub struct ParsedJob {
    pub subshell_type: SubshellType,
    pub jobs: Vec<Job>,
}

impl ParsedJob {
    pub fn new(subshell_type: SubshellType, jobs: Vec<Job>) -> Self {
        Self {
            subshell_type,
            jobs,
        }
    }
}

#[derive(Debug)]
pub struct ParseContext {
    pub foreground: bool,
    pub subshell: bool,
    pub proc_subst: bool,
}

impl ParseContext {
    pub fn new(foreground: bool) -> Self {
        Self {
            foreground,
            subshell: false,
            proc_subst: false,
        }
    }
}

/// `FOO=bar` with no command sets a shell variable, the way `set` does.
///
/// Not exported: a prefix only reaches a child process when there is a command
/// for it to run.
fn apply_standalone_assignments(shell: &mut Shell, assignments: &mut Vec<(String, String)>) {
    if assignments.is_empty() {
        return;
    }
    let mut env = shell.environment.write();
    for (name, value) in assignments.drain(..) {
        env.variable_state.variables.insert(name, value);
    }
}

/// Split one `NAME=value` into its two halves.
///
/// The value goes through `get_string` like any other word, so quoting and
/// escapes behave the same as they would in an argument. A bare `NAME=` is an
/// empty value, which is how shells spell "defined but empty".
fn parse_assignment(pair: Pair<Rule>) -> (String, String) {
    let mut name = String::new();
    let mut value = String::new();
    for part in pair.into_inner() {
        match part.as_rule() {
            Rule::assign_name => name = part.as_str().to_string(),
            Rule::span => value = parser::get_string(part).unwrap_or_default(),
            _ => {}
        }
    }
    (name, value)
}

/// Whether any part of this span is a substitution.
///
/// Substitutions become their own argv entries because they can expand to
/// several words; everything else in a span is joined into one argument.
fn span_has_substitution(span: &Pair<Rule>) -> bool {
    span.clone().into_inner().any(|part| {
        matches!(
            part.as_rule(),
            Rule::subshell | Rule::proc_subst | Rule::command_subst
        )
    })
}

/// Attach the command's redirections and environment prefix, then add it
/// to the job.
///
/// Per process rather than per job: in `a 2>&1 | b` the duplication belongs to
/// `a`, and applying it to `b` sent the error to the terminal instead of down
/// the pipe.
fn attach_process(
    job: &mut Job,
    mut process: JobProcess,
    redirects: &mut Vec<Redirect>,
    env_overrides: &mut Vec<(String, String)>,
) {
    process.set_redirects(std::mem::take(redirects));
    process.set_env_overrides(std::mem::take(env_overrides));
    job.set_process(process);
}

/// Build the redirections one `redirect` pair stands for.
///
/// `&>` desugars into two entries, which is why this returns a list: keeping
/// the ordering explicit is what makes `> f 2>&1` and `2>&1 > f` differ
/// correctly once the list is applied left to right.
fn parse_redirect(pair: Pair<Rule>) -> Result<Vec<Redirect>> {
    let mut direction = None;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::fd_dup => return parse_fd_dup(inner),
            Rule::stdout_redirect_direction
            | Rule::stderr_redirect_direction
            | Rule::stdouterr_redirect_direction
            | Rule::stdin_redirect_direction => {
                direction = inner.into_inner().next().map(|rule| rule.as_rule());
            }
            Rule::span => {
                // Through `get_string` so the target goes through the same
                // quote removal and expansion as any other word -- taking the
                // raw text left the quotes in `> "my file.txt"`.
                let dest = parser::get_string(inner).unwrap_or_default();
                return Ok(match direction {
                    Some(Rule::stdout_redirect_direction_out) => {
                        vec![Redirect::write(STDOUT_FILENO, dest)]
                    }
                    Some(Rule::stdout_redirect_direction_append) => {
                        vec![Redirect::append(STDOUT_FILENO, dest)]
                    }
                    Some(Rule::stderr_redirect_direction_out) => {
                        vec![Redirect::write(STDERR_FILENO, dest)]
                    }
                    Some(Rule::stderr_redirect_direction_append) => {
                        vec![Redirect::append(STDERR_FILENO, dest)]
                    }
                    Some(Rule::stdouterr_redirect_direction_out) => Redirect::both(dest, false),
                    Some(Rule::stdouterr_redirect_direction_append) => Redirect::both(dest, true),
                    Some(Rule::stdin_redirect_direction_in) => vec![Redirect::input(dest)],
                    _ => Vec::new(),
                });
            }
            _ => {}
        }
    }

    Ok(Vec::new())
}

/// `2>&1`, `>&2`, `2>&-`.
fn parse_fd_dup(pair: Pair<Rule>) -> Result<Vec<Redirect>> {
    let Some(form) = pair.into_inner().next() else {
        return Ok(Vec::new());
    };
    // Without an explicit number, `>&` means stdout and `<&` means stdin.
    let default_fd = match form.as_rule() {
        Rule::fd_dup_in => STDIN_FILENO,
        _ => STDOUT_FILENO,
    };

    let mut fd = default_fd;
    let mut target = None;
    for inner in form.into_inner() {
        match inner.as_rule() {
            Rule::fd_number => fd = parse_fd(inner.as_str())?,
            Rule::fd_dup_target => target = Some(inner.as_str().to_string()),
            _ => {}
        }
    }

    let Some(target) = target else {
        return Ok(Vec::new());
    };
    Ok(vec![if target == "-" {
        Redirect::close(fd)
    } else {
        Redirect::dup(fd, parse_fd(&target)?)
    }])
}

fn parse_fd(text: &str) -> Result<RawFd> {
    text.parse::<RawFd>()
        .with_context(|| format!("dsh: invalid file descriptor '{text}'"))
}

pub fn parse_argv(
    shell: &mut Shell,
    ctx: &mut ParseContext,
    current_job: &mut Job,
    pair: Pair<Rule>,
) -> Result<Vec<(String, Option<ParsedJob>)>> {
    let mut argv: Vec<(String, Option<ParsedJob>)> = vec![];

    for inner_pair in pair.into_inner() {
        match inner_pair.as_rule() {
            Rule::argv0 => {
                for inner_pair in inner_pair.into_inner() {
                    // span
                    // A span is one argument. Only a substitution needs its
                    // parts handled separately; everything else is joined by
                    // `get_string`, or `echo a"b"c` would arrive as three
                    // arguments.
                    if !span_has_substitution(&inner_pair) {
                        if let Some(arg) = parser::get_string(inner_pair) {
                            argv.push((arg, None));
                        }
                        continue;
                    }

                    for inner_pair in inner_pair.into_inner() {
                        match inner_pair.as_rule() {
                            Rule::subshell => {
                                debug!("find subshell arg0");
                                for inner_pair in inner_pair.into_inner() {
                                    // commands
                                    let cmd_str = inner_pair.as_str().to_string();
                                    // subshell
                                    let mut ctx = ParseContext::new(ctx.foreground);
                                    ctx.subshell = true;
                                    let res = parse_commands(shell, &mut ctx, inner_pair)?;
                                    argv.push((
                                        cmd_str,
                                        Some(ParsedJob::new(SubshellType::Subshell, res)),
                                    ));
                                }
                            }
                            Rule::proc_subst => {
                                for inner_pair in inner_pair.into_inner() {
                                    // commands
                                    let cmd_str = inner_pair.as_str().to_string();
                                    let mut ctx = ParseContext::new(ctx.foreground);
                                    ctx.proc_subst = true;
                                    let res = parse_commands(shell, &mut ctx, inner_pair)?;
                                    argv.push((
                                        cmd_str,
                                        Some(ParsedJob::new(
                                            SubshellType::ProcessSubstitution,
                                            res,
                                        )),
                                    ));
                                }
                            }
                            Rule::command_subst => {
                                for inner_pair in inner_pair.into_inner() {
                                    let cmd_str = inner_pair.as_str().to_string();
                                    let mut ctx = ParseContext::new(ctx.foreground);
                                    ctx.subshell = true;
                                    let res = parse_commands(shell, &mut ctx, inner_pair)?;
                                    argv.push((
                                        cmd_str,
                                        Some(ParsedJob::new(
                                            SubshellType::CommandSubstitution,
                                            res,
                                        )),
                                    ));
                                }
                            }
                            _ => {
                                if let Some(arg) = parser::get_string(inner_pair) {
                                    argv.push((arg, None));
                                }
                            }
                        }
                    }
                }
            }
            Rule::assignment_list => {
                for assignment in inner_pair.into_inner() {
                    current_job.env_overrides.push(parse_assignment(assignment));
                }
            }
            Rule::args => {
                for inner_pair in inner_pair.into_inner() {
                    if let Rule::redirect = inner_pair.as_rule() {
                        current_job.redirects.extend(parse_redirect(inner_pair)?);
                        continue;
                    }

                    // A span is one argument. Only a substitution needs its
                    // parts handled separately; everything else is joined by
                    // `get_string`, or `echo a"b"c` would arrive as three
                    // arguments.
                    if !span_has_substitution(&inner_pair) {
                        if let Some(arg) = parser::get_string(inner_pair) {
                            argv.push((arg, None));
                        }
                        continue;
                    }

                    for inner_pair in inner_pair.into_inner() {
                        match inner_pair.as_rule() {
                            Rule::subshell => {
                                debug!("find subshell args");
                                for inner_pair in inner_pair.into_inner() {
                                    // commands
                                    let cmd_str = inner_pair.as_str().to_string();
                                    // subshell
                                    let mut ctx = ParseContext::new(ctx.foreground);
                                    ctx.subshell = true;
                                    let res = parse_commands(shell, &mut ctx, inner_pair)?;
                                    argv.push((
                                        cmd_str,
                                        Some(ParsedJob::new(SubshellType::Subshell, res)),
                                    ));
                                }
                            }
                            Rule::proc_subst => {
                                debug!("find proc_subs args");
                                for inner_pair in inner_pair.into_inner() {
                                    if inner_pair.as_rule() == Rule::proc_subst_direction {
                                        continue;
                                    }
                                    // commands
                                    let cmd_str = inner_pair.as_str().to_string();
                                    let mut ctx = ParseContext::new(ctx.foreground);
                                    ctx.proc_subst = true;
                                    let res = parse_commands(shell, &mut ctx, inner_pair)?;
                                    argv.push((
                                        cmd_str,
                                        Some(ParsedJob::new(
                                            SubshellType::ProcessSubstitution,
                                            res,
                                        )),
                                    ));
                                }
                            }
                            Rule::command_subst => {
                                debug!("find command_subst args");
                                for inner_pair in inner_pair.into_inner() {
                                    let cmd_str = inner_pair.as_str().to_string();
                                    let mut ctx = ParseContext::new(ctx.foreground);
                                    ctx.subshell = true;
                                    let res = parse_commands(shell, &mut ctx, inner_pair)?;
                                    argv.push((
                                        cmd_str,
                                        Some(ParsedJob::new(
                                            SubshellType::CommandSubstitution,
                                            res,
                                        )),
                                    ));
                                }
                            }
                            _ => {
                                if let Some(arg) = parser::get_string(inner_pair) {
                                    argv.push((arg, None));
                                }
                            }
                        }
                    }
                }
            }
            Rule::simple_command => {
                let mut res = parse_argv(shell, ctx, current_job, inner_pair)?;
                argv.append(&mut res);
            }
            _ => {
                warn!("missing {:?}", inner_pair.as_rule());
            }
        }
    }
    Ok(argv)
}

pub fn parse_commands(
    shell: &mut Shell,
    ctx: &mut ParseContext,
    pair: Pair<Rule>,
) -> Result<Vec<Job>> {
    let mut jobs: Vec<Job> = Vec::new();
    if let Rule::commands = pair.as_rule() {
        for pair in pair.into_inner() {
            match pair.as_rule() {
                Rule::command => parse_jobs(shell, ctx, pair, &mut jobs)?,
                Rule::command_list_sep => {
                    if let Some(sep) = pair.into_inner().next()
                        && let Some(ref mut last) = jobs.last_mut()
                    {
                        debug!("last job {:?}", &last.cmd);
                        match sep.as_rule() {
                            Rule::and_op => {
                                last.list_op = process::ListOp::And;
                            }
                            Rule::or_op => {
                                last.list_op = process::ListOp::Or;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {
                    debug!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                }
            }
        }
    }

    debug!("parsed jobs len: {}", jobs.len());
    Ok(jobs)
}

pub fn parse_command(
    shell: &mut Shell,
    ctx: &mut ParseContext,
    current_job: &mut Job,
    pair: Pair<Rule>,
) -> Result<()> {
    debug!("start parse command: {}", pair.as_str());
    let parsed_argv = parse_argv(shell, ctx, current_job, pair)?;
    // `parse_argv` collects redirections on the job as a staging area; they
    // belong to the command being built here, so move them across.
    let mut redirects = std::mem::take(&mut current_job.redirects);
    let mut env_overrides = std::mem::take(&mut current_job.env_overrides);
    if parsed_argv.is_empty() {
        apply_standalone_assignments(shell, &mut env_overrides);
        return Ok(());
    }

    let mut argv: Vec<String> = Vec::new();

    for (cmd_str, jobs) in parsed_argv {
        if let Some(ParsedJob {
            subshell_type,
            jobs,
        }) = jobs
        {
            debug!("parsed job '{:?}' jobs:{:?}", cmd_str, jobs);
            if jobs.is_empty() {
                continue;
            }
            debug!("run subshell: {}", cmd_str);
            let tmode = match tcgetattr(unsafe { BorrowedFd::borrow_raw(0) }) {
                Ok(mode) => Some(mode),
                Err(err) => {
                    debug!("tcgetattr fallback for command substitution: {}", err);
                    Context::new_safe(shell.pid, shell.pgid, false).shell_tmode
                }
            };

            match subshell_type {
                SubshellType::Subshell => {
                    let ctx = Context::new(shell.pid, shell.pgid, tmode.clone(), false);
                    let output = shell.capture_subshell_stdout(&ctx, jobs)?;
                    output.lines().for_each(|x| argv.push(x.to_owned()));
                }
                SubshellType::CommandSubstitution => {
                    let ctx = Context::new(shell.pid, shell.pgid, tmode.clone(), false);
                    let output = shell.capture_subshell_stdout(&ctx, jobs)?;
                    for part in output.split_whitespace() {
                        if !part.is_empty() {
                            argv.push(part.to_owned());
                        }
                    }
                }
                SubshellType::ProcessSubstitution => {
                    let mut ctx = Context::new(shell.pid, shell.pgid, tmode.clone(), false);
                    ctx.foreground = true;
                    // Deliberately NOT `cloexec_pipe`: the read end is handed to
                    // the command as `/dev/fd/N`, so it has to survive its exec.
                    let (pout, pin) = pipe().context("failed pipe")?;
                    ctx.outfile = pin.as_raw_fd();
                    shell.launch_subshell(&mut ctx, jobs)?;
                    drop(pin); // Close write end
                    // Leak pout to keep it open for process substitution
                    let file_name = format!("/dev/fd/{}", pout.into_raw_fd());
                    argv.push(file_name);
                }
                SubshellType::None => {}
            }
        } else {
            argv.push(cmd_str);
        }
    }

    if argv.is_empty() {
        apply_standalone_assignments(shell, &mut env_overrides);
        // no main command
        return Ok(());
    }

    // Handle 'nopty' prefix
    if argv[0] == "nopty" {
        if argv.len() > 1 {
            argv.remove(0);
            current_job.disable_pty = true;
            debug!("'nopty' detected, disabling PTY for this job");
        } else {
            // "nopty" with no command? Just ignore it or treat as command "nopty" which likely fails
        }
    }

    let cmd = argv[0].as_str();
    // A builtin runs inside the shell for a foreground job, so a per-command
    // environment would have to be applied and unwound around the call. Say so
    // rather than accepting the prefix and quietly ignoring it.
    if !env_overrides.is_empty()
        && (dsh_builtin::get_handler(cmd).is_some() || shell.lisp_engine.borrow().is_export(cmd))
    {
        // Report and skip *this* command. Bailing here aborted the whole line,
        // so `FOO=bar cd /tmp; echo ok` silently dropped `echo ok` as well.
        eprintln!("dsh: {cmd}: a NAME=value prefix is not supported for builtins");
        return Ok(());
    }

    if let Some(handler) = dsh_builtin::get_handler(cmd) {
        let builtin = process::BuiltinProcess::new_handler(cmd.to_string(), handler, argv);
        attach_process(
            current_job,
            JobProcess::Builtin(builtin),
            &mut redirects,
            &mut env_overrides,
        );
    } else if shell.lisp_engine.borrow().is_export(cmd) {
        let cmd_fn = dsh_builtin::lisp::run;
        let builtin = process::BuiltinProcess::new(cmd.to_string(), cmd_fn, argv);
        attach_process(
            current_job,
            JobProcess::Builtin(builtin),
            &mut redirects,
            &mut env_overrides,
        );
    } else {
        let cmd_lookup = shell.environment.read().lookup(cmd);
        if let Some(cmd) = cmd_lookup {
            let process = process::Process::new(cmd, argv);
            attach_process(
                current_job,
                JobProcess::Command(process),
                &mut redirects,
                &mut env_overrides,
            );
            current_job.foreground = ctx.foreground;
        } else if dirs::is_dir(cmd) {
            if let Some(handler) = dsh_builtin::get_handler("cd") {
                let builtin = process::BuiltinProcess::new_handler(
                    cmd.to_string(),
                    handler,
                    vec!["cd".to_string(), cmd.to_string()],
                );
                attach_process(
                    current_job,
                    JobProcess::Builtin(builtin),
                    &mut redirects,
                    &mut env_overrides,
                );
            }
        } else {
            // Execute command-not-found hooks before showing error
            // Hooks can perform side effects like suggesting package installation
            shell.exec_command_not_found_hooks(cmd);

            // Try to find similar commands for suggestion
            let paths = shell.environment.read().variable_state.paths.clone();
            let builtins: Vec<String> = dsh_builtin::get_all_commands()
                .iter()
                .map(|(name, _)| name.to_string())
                .collect();

            let suggestions =
                crate::command_suggestion::find_similar_commands(cmd, &paths, &builtins);
            let task_suggestions = std::env::current_dir()
                .ok()
                .and_then(|cwd| dsh_builtin::task::list_tasks_in_dir(&cwd).ok())
                .map(|tasks| {
                    let task_names: Vec<String> = tasks.into_iter().map(|task| task.name).collect();
                    crate::command_suggestion::find_similar_candidates(cmd, &task_names)
                })
                .unwrap_or_default();

            let suggestion_msg =
                crate::command_suggestion::format_suggestions(&suggestions).unwrap_or_default();
            let task_msg = if task_suggestions.is_empty() {
                String::new()
            } else {
                let commands = task_suggestions
                    .iter()
                    .map(|suggestion| format!("task {}", suggestion.command))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("\rProject tasks: {commands}\r\n")
            };

            if !suggestion_msg.is_empty() || !task_msg.is_empty() {
                bail!("unknown command: {}\n{}{}", cmd, suggestion_msg, task_msg);
            }

            bail!("unknown command: {}", cmd);
        }
    }
    Ok(())
}

fn parse_jobs(
    shell: &mut Shell,
    ctx: &mut ParseContext,
    pair: Pair<Rule>,
    jobs: &mut Vec<Job>,
) -> Result<()> {
    let job_str = pair.as_str().to_string();

    for inner_pair in pair.into_inner() {
        debug!(
            "find {:?}:'{:?}'",
            inner_pair.as_rule(),
            inner_pair.as_str()
        );
        match inner_pair.as_rule() {
            Rule::simple_command => {
                let mut job = Job::new(job_str.clone(), shell.pgid);
                job.job_id = shell.get_next_job_id();
                parse_command(shell, ctx, &mut job, inner_pair)?;
                if job.has_process() {
                    if ctx.subshell {
                        job.subshell = SubshellType::Subshell;
                    }
                    if ctx.proc_subst {
                        job.subshell = SubshellType::ProcessSubstitution;
                    }
                    jobs.push(job);
                }
            }
            Rule::simple_command_bg => {
                // background job
                let mut job = Job::new(inner_pair.as_str().to_string(), shell.pgid);
                job.job_id = shell.get_next_job_id();
                for bg_pair in inner_pair.into_inner() {
                    if let Rule::simple_command = bg_pair.as_rule() {
                        parse_command(shell, ctx, &mut job, bg_pair)?;
                        if job.has_process() {
                            if ctx.subshell {
                                job.subshell = SubshellType::Subshell;
                            }
                            if ctx.proc_subst {
                                job.subshell = SubshellType::ProcessSubstitution;
                            }
                            job.foreground = false; // background
                            jobs.push(job);
                        }
                        break;
                    }
                }
            }
            Rule::pipe_command => {
                // For pipe commands, create a new job if no existing job
                if jobs.is_empty() {
                    let mut job = Job::new(job_str.clone(), shell.pgid);
                    job.job_id = shell.get_next_job_id();
                    if ctx.subshell {
                        job.subshell = SubshellType::Subshell;
                    }
                    if ctx.proc_subst {
                        job.subshell = SubshellType::ProcessSubstitution;
                    }
                    jobs.push(job);
                }

                if let Some(job) = jobs.last_mut() {
                    for inner_pair in inner_pair.into_inner() {
                        let _cmd = inner_pair.as_str();
                        if let Rule::simple_command = inner_pair.as_rule() {
                            ctx.foreground = true;
                            parse_command(shell, ctx, job, inner_pair)?;
                        } else if let Rule::simple_command_bg = inner_pair.as_rule() {
                            ctx.foreground = false;
                            parse_command(shell, ctx, job, inner_pair)?;
                        } else {
                            // TODO check?
                        }
                    }
                }
            }
            Rule::capture_suffix => {
                // Set capture_output flag on the last job
                if let Some(job) = jobs.last_mut() {
                    job.capture_output = true;
                    debug!("Capture mode enabled for job: {}", job.cmd);
                }
            }
            Rule::struct_pipe_command => {
                // Extract Lisp expression from struct_pipe_command
                // The rule is: struct_pipe_command = { struct_pipe_op ~ sp* ~ lisp_expr ~ sp* }
                for inner_pair in inner_pair.into_inner() {
                    if let Rule::lisp_expr = inner_pair.as_rule() {
                        let lisp_expr = inner_pair.as_str().to_string();
                        debug!("Found struct_pipe Lisp expression: {}", lisp_expr);

                        // Add to last job's struct_pipe_exprs or create new job
                        if let Some(job) = jobs.last_mut() {
                            job.struct_pipe_exprs.push(lisp_expr);
                        } else {
                            let mut job = Job::new(job_str.clone(), shell.pgid);
                            job.job_id = shell.get_next_job_id();
                            job.struct_pipe_exprs.push(lisp_expr);
                            jobs.push(job);
                        }
                    }
                }
            }
            _ => {
                warn!(
                    "missing rule {:?} {:?}",
                    inner_pair.as_rule(),
                    inner_pair.as_str()
                );
            }
        }
    }
    Ok(())
}
