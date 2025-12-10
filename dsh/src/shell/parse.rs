use crate::dirs;
use crate::parser::{self, Rule};
use crate::process::{self, Job, JobProcess, Redirect, SubshellType};
use crate::shell::Shell;
use anyhow::{Context as _, Result, anyhow, bail};
use dsh_types::Context;
use nix::sys::termios::tcgetattr;
use nix::unistd::{close, pipe};
use pest::iterators::Pair;
use std::fs::File;
use std::io::Read;
use std::os::unix::io::FromRawFd;
use std::os::unix::io::RawFd;
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
            Rule::args => {
                for inner_pair in inner_pair.into_inner() {
                    if let Rule::redirect = inner_pair.as_rule() {
                        // set redirect
                        let mut redirect_rule = None;
                        for pair in inner_pair.into_inner() {
                            if let Rule::stdout_redirect_direction
                            | Rule::stderr_redirect_direction
                            | Rule::stdouterr_redirect_direction
                            | Rule::stdin_redirect_direction = pair.as_rule()
                            {
                                if let Some(rule) = pair.into_inner().next() {
                                    redirect_rule = Some(rule.as_rule());
                                }
                            } else if let Rule::span = pair.as_rule() {
                                let dest = pair.as_str();

                                let redirect = match redirect_rule {
                                    Some(Rule::stdout_redirect_direction_out) => {
                                        Some(Redirect::StdoutOutput(dest.to_string()))
                                    }
                                    Some(Rule::stdout_redirect_direction_append) => {
                                        Some(Redirect::StdoutAppend(dest.to_string()))
                                    }

                                    Some(Rule::stderr_redirect_direction_out) => {
                                        Some(Redirect::StderrOutput(dest.to_string()))
                                    }
                                    Some(Rule::stderr_redirect_direction_append) => {
                                        Some(Redirect::StderrAppend(dest.to_string()))
                                    }

                                    Some(Rule::stdouterr_redirect_direction_out) => {
                                        Some(Redirect::StdouterrOutput(dest.to_string()))
                                    }
                                    Some(Rule::stdouterr_redirect_direction_append) => {
                                        Some(Redirect::StdouterrAppend(dest.to_string()))
                                    }
                                    Some(Rule::stdin_redirect_direction_in) => {
                                        Some(Redirect::Input(dest.to_string()))
                                    }
                                    _ => None,
                                };
                                current_job.redirect = redirect;
                            }
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
    if parsed_argv.is_empty() {
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
            let tmode = match tcgetattr(0) {
                Ok(mode) => mode,
                Err(err) => {
                    debug!("tcgetattr fallback for command substitution: {}", err);
                    Context::new_safe(shell.pid, shell.pgid, false).shell_tmode
                }
            };

            match subshell_type {
                SubshellType::Subshell => {
                    let mut ctx = Context::new(shell.pid, shell.pgid, tmode, false);
                    ctx.foreground = true;
                    // make pipe
                    let (pout, pin) = pipe().context("failed pipe")?;
                    ctx.outfile = pin;
                    shell.launch_subshell(&mut ctx, jobs)?;
                    close(pin).map_err(|e| anyhow!("failed to close pipe: {}", e))?;
                    let output = read_fd(pout)?;
                    output.lines().for_each(|x| argv.push(x.to_owned()));
                }
                SubshellType::CommandSubstitution => {
                    let mut ctx = Context::new(shell.pid, shell.pgid, tmode, false);
                    ctx.foreground = true;
                    let (pout, pin) = pipe().context("failed pipe")?;
                    ctx.outfile = pin;
                    shell.launch_subshell(&mut ctx, jobs)?;
                    close(pin).map_err(|e| anyhow!("failed to close pipe: {}", e))?;
                    let output = read_fd(pout)?;
                    for part in output.split_whitespace() {
                        if !part.is_empty() {
                            argv.push(part.to_owned());
                        }
                    }
                }
                SubshellType::ProcessSubstitution => {
                    let mut ctx = Context::new(shell.pid, shell.pgid, tmode, false);
                    ctx.foreground = true;
                    // make pipe
                    let (pout, pin) = pipe().context("failed pipe")?;
                    ctx.outfile = pin;
                    shell.launch_subshell(&mut ctx, jobs)?;
                    close(pin).map_err(|e| anyhow!("failed to close pipe: {}", e))?;
                    let file_name = format!("/dev/fd/{pout}");
                    argv.push(file_name);
                }
                SubshellType::None => {}
            }
        } else {
            argv.push(cmd_str);
        }
    }

    if argv.is_empty() {
        // no main command
        return Ok(());
    }

    let cmd = argv[0].as_str();
    if let Some(cmd_fn) = dsh_builtin::get_command(cmd) {
        let builtin = process::BuiltinProcess::new(cmd.to_string(), cmd_fn, argv);
        current_job.set_process(JobProcess::Builtin(builtin));
    } else if shell.lisp_engine.borrow().is_export(cmd) {
        let cmd_fn = dsh_builtin::lisp::run;
        let builtin = process::BuiltinProcess::new(cmd.to_string(), cmd_fn, argv);
        current_job.set_process(JobProcess::Builtin(builtin));
    } else {
        let cmd_lookup = shell.environment.read().lookup(cmd);
        if let Some(cmd) = cmd_lookup {
            let process = process::Process::new(cmd, argv);
            current_job.set_process(JobProcess::Command(process));
            current_job.foreground = ctx.foreground;
        } else if dirs::is_dir(cmd) {
            if let Some(cmd_fn) = dsh_builtin::get_command("cd") {
                let builtin = process::BuiltinProcess::new(
                    cmd.to_string(),
                    cmd_fn,
                    vec!["cd".to_string(), cmd.to_string()],
                );
                current_job.set_process(JobProcess::Builtin(builtin));
            }
        } else {
            // Execute command-not-found hooks before showing error
            // Hooks can perform side effects like suggesting package installation
            shell.exec_command_not_found_hooks(cmd);

            // Try to find similar commands for suggestion
            let paths = shell.environment.read().paths.clone();
            let builtins: Vec<String> = dsh_builtin::get_all_commands()
                .iter()
                .map(|(name, _)| name.to_string())
                .collect();

            let suggestions =
                crate::command_suggestion::find_similar_commands(cmd, &paths, &builtins);

            if let Some(suggestion_msg) =
                crate::command_suggestion::format_suggestions(&suggestions)
            {
                bail!("unknown command: {}\n{}", cmd, suggestion_msg);
            } else {
                bail!("unknown command: {}", cmd);
            }
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

fn read_fd(fd: RawFd) -> Result<String> {
    let mut raw_stdout = Vec::new();
    unsafe {
        File::from_raw_fd(fd)
            .read_to_end(&mut raw_stdout)
            .context("failed to read from fd")?;
    };

    let output = std::str::from_utf8(&raw_stdout)
        .inspect_err(|_err| {
            // TODO
            eprintln!("binary in variable/expansion is not supported");
        })?
        .trim_end_matches('\n')
        .to_owned();
    Ok(output)
}
