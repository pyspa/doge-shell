use anyhow::Result;
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::unistd::{Pid, close, getpgid, getpgrp, setpgid};
use std::os::unix::io::RawFd;
use tracing::{debug, error};

use super::io::OutputMonitor;
use super::job_process::JobProcess;
use super::process::Process;
use super::redirect::{self, Redirect};
use super::state::{ListOp, ProcessState, SubshellType};
use super::wait::is_job_completed;
use crate::process::pty::{Pty, PtyChildConfig, PtyMode};
use crate::shell::Shell;
use dsh_types::Context;

use crate::process::job_pty;
use crate::process::job_wait;

#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub cmd: String,
    pub pid: Option<Pid>,
    pub pgid: Option<Pid>,
    pub(crate) process: Option<Box<JobProcess>>,
    stdin: RawFd,
    stdout: RawFd,
    stderr: RawFd,
    pub foreground: bool,
    pub subshell: SubshellType,
    /// Redirections in the order they were written; applied left to right.
    pub redirects: Vec<Redirect>,
    /// `NAME=value` written before the command, staged here by the parser
    /// and moved onto the process it belongs to.
    pub env_overrides: Vec<(String, String)>,
    pub list_op: ListOp,
    pub job_id: usize,
    pub state: ProcessState,
    pub(crate) monitors: Vec<OutputMonitor>,
    pub(crate) shell_pgid: Pid,
    /// Whether to capture output for $OUT variable
    pub capture_output: bool,
    pub pty: Option<Pty>,
    pub(crate) pty_mode: Option<PtyMode>,
    pub pty_output_task: Option<tokio::task::JoinHandle<Result<String>>>,
    pub pty_input_task: Option<tokio::task::JoinHandle<()>>,
    pub disable_pty: bool,
    /// Lisp expressions to evaluate after command output (from |: operator)
    pub struct_pipe_exprs: Vec<String>,
    /// When the job was created, used to decide whether a finished background
    /// job ran long enough to warrant a desktop notification.
    pub started_at: std::time::Instant,
}

fn last_process_state(process: JobProcess) -> ProcessState {
    debug!(
        "last_process_state:{} {} has_next: {}",
        process.get_cmd(),
        process.get_state(),
        process.next().is_some(),
    );
    if let Some(next_proc) = process.next() {
        last_process_state(*next_proc)
    } else {
        process.get_state()
    }
}

impl Job {
    pub fn new_with_process(cmd: String, path: String, argv: Vec<String>) -> Self {
        let process = JobProcess::Command(Process::new(path, argv));
        let id = format!("{}", xid::new());
        let shell_pgid = getpgrp();
        Job {
            id,
            cmd,
            pid: None,
            pgid: None,
            process: Some(Box::new(process)),
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            foreground: true,
            subshell: SubshellType::None,
            redirects: Vec::new(),
            env_overrides: Vec::new(),
            list_op: ListOp::None,
            job_id: 1,
            state: ProcessState::Running,
            monitors: Vec::new(),
            shell_pgid,
            capture_output: false,
            pty: None,
            pty_mode: None,
            pty_output_task: None,
            pty_input_task: None,
            disable_pty: false,
            struct_pipe_exprs: Vec::new(),
            started_at: std::time::Instant::now(),
        }
    }

    pub fn new(cmd: String, shell_pgid: Pid) -> Self {
        let id = format!("{}", xid::new());
        Job {
            id,
            cmd,
            pid: None,
            pgid: None,
            process: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            foreground: true,
            subshell: SubshellType::None,
            redirects: Vec::new(),
            env_overrides: Vec::new(),
            list_op: ListOp::None,
            job_id: 1,
            state: ProcessState::Running,
            monitors: Vec::new(),
            shell_pgid,
            capture_output: false,
            pty: None,
            pty_mode: None,
            pty_output_task: None,
            pty_input_task: None,
            disable_pty: false,
            struct_pipe_exprs: Vec::new(),
            started_at: std::time::Instant::now(),
        }
    }

    pub fn has_process(&self) -> bool {
        self.process.is_some()
    }

    pub fn set_process(&mut self, process: JobProcess) {
        match self.process {
            Some(ref mut p) => p.link(process),
            None => self.process = Some(Box::new(process)),
        }
    }

    /// argv of the pipeline's last process when it is an external command.
    ///
    /// `|:` captures that process's stdout, so it is the one output schemas
    /// match against; a builtin tail returns `None`.
    pub(crate) fn last_external_argv(&self) -> Option<Vec<String>> {
        let mut current = self.process.as_deref()?;
        loop {
            let next = match current {
                JobProcess::Builtin(process) => process.next.as_deref(),
                JobProcess::Command(process) => process.next.as_deref(),
            };
            match next {
                Some(next) => current = next,
                None => break,
            }
        }
        match current {
            JobProcess::Command(process) => Some(process.argv.clone()),
            JobProcess::Builtin(_) => None,
        }
    }

    /// Insert arguments into the pipeline's last external command (an output
    /// schema's `inject_args`). A builtin tail is left untouched.
    ///
    /// Arguments go *before* a standalone `--`: everything after that
    /// terminator is operands (`git log -- <path>`), so appending there would
    /// turn injected flags into pathspecs and break the command.
    pub(crate) fn append_args_to_last_external(&mut self, args: &[String]) {
        let Some(mut current) = self.process.as_deref_mut() else {
            return;
        };
        loop {
            let has_next = match &*current {
                JobProcess::Builtin(process) => process.next.is_some(),
                JobProcess::Command(process) => process.next.is_some(),
            };
            if !has_next {
                break;
            }
            current = match current {
                JobProcess::Builtin(process) => process.next.as_deref_mut().unwrap(),
                JobProcess::Command(process) => process.next.as_deref_mut().unwrap(),
            };
        }
        if let JobProcess::Command(process) = current {
            // argv[0] is the command name, so start the search at 1.
            let at = process
                .argv
                .iter()
                .skip(1)
                .position(|arg| arg == "--")
                .map(|index| index + 1)
                .unwrap_or(process.argv.len());
            process.argv.splice(at..at, args.iter().cloned());
        }
    }

    pub fn last_process_state(&self) -> ProcessState {
        if let Some(p) = &self.process {
            last_process_state(*p.clone())
        } else {
            // not running
            ProcessState::Completed(0, None)
        }
    }

    pub async fn launch(&mut self, ctx: &mut Context, shell: &mut Shell) -> Result<ProcessState> {
        // Record the descriptors the caller handed us. They belong to the
        // caller — `execute_with_capture` and `$( )` both pass a pipe they read
        // themselves — so the pipeline default restores to them and the
        // per-process cleanup below must never close them. Closing the caller's
        // stderr is what broke `a | b |>`, and closing the caller's stdout is
        // what made `$(a; b)` lose everything after the first command.
        self.stdin = ctx.infile;
        self.stdout = ctx.outfile;
        self.stderr = ctx.errfile;

        let result = self.launch_inner(ctx, shell).await;

        // Launching rewires `ctx` (pipes, capture, redirections) and nothing put
        // it back. Script mode reuses one `ctx` for every line, so the next line
        // inherited a descriptor this job had already closed and `2>&1` failed
        // with `failed to duplicate file descriptor`.
        ctx.infile = self.stdin;
        ctx.outfile = self.stdout;
        ctx.errfile = self.stderr;

        result
    }

    async fn launch_inner(&mut self, ctx: &mut Context, shell: &mut Shell) -> Result<ProcessState> {
        debug!(
            "JOB_LAUNCH_START: Starting job {} launch (cmd: '{}', foreground: {}, pid: {:?})",
            self.job_id, self.cmd, self.foreground, self.pid
        );

        ctx.foreground = self.foreground;

        // 1. Setup PTY if needed
        let pty_slave_fd = self.setup_pty(ctx).await?;
        let pty_child = pty_slave_fd.map(|slave| PtyChildConfig {
            slave,
            mode: self.pty_mode.unwrap_or(PtyMode::FullProxy),
        });
        let _pty_raw_mode_guard = job_pty::ForegroundPtyRawModeGuard::new(self, ctx);

        // 2. Launch processes
        if let Some(mut process) = self.process.take() {
            debug!(
                "JOB_LAUNCH_PROCESS: Launching process for job {} (process_type: {})",
                self.job_id,
                process.get_cmd()
            );

            if let Err(e) = self
                .launch_process(ctx, shell, &mut process, pty_child)
                .await
            {
                error!(
                    "JOB_LAUNCH_PROCESS_ERROR: Failed to launch process for job {}: {}",
                    self.job_id, e
                );
                self.cleanup_pty_tasks().await;
                return Err(e);
            }

            // 3. Manage execution (Foreground/Background)
            self.manage_execution(ctx).await?;
        } else {
            debug!(
                "JOB_LAUNCH_NO_PROCESS: Job {} has no process to launch",
                self.job_id
            );
        }

        // 4. Capture output and save to history
        self.capture_output_and_history(ctx, shell).await?;

        let final_state = if ctx.foreground {
            self.last_process_state()
        } else {
            ProcessState::Running
        };

        debug!(
            "JOB_LAUNCH_RESULT: Job {} launch result - state: {:?}, foreground: {}",
            self.job_id, final_state, ctx.foreground
        );

        Ok(final_state)
    }

    pub(crate) async fn setup_pty(&mut self, ctx: &mut Context) -> Result<Option<RawFd>> {
        job_pty::setup_pty(self, ctx).await
    }

    pub(crate) async fn cleanup_pty_tasks(&mut self) {
        job_pty::cleanup_pty_tasks(self).await
    }

    async fn manage_execution(&mut self, ctx: &mut Context) -> Result<()> {
        job_pty::manage_execution(self, ctx).await
    }

    async fn capture_output_and_history(&mut self, ctx: &Context, shell: &mut Shell) -> Result<()> {
        job_pty::capture_output_and_history(self, ctx, shell).await
    }

    async fn launch_process(
        &mut self,
        ctx: &mut Context,
        shell: &mut Shell,
        process: &mut JobProcess,
        pty: Option<PtyChildConfig>,
    ) -> Result<()> {
        let previous_infile = ctx.infile;
        // Input redirection is applied here, before the process is launched;
        // the output side is applied inside `launch`, after the pipe and PTY
        // wiring, so `2>&1` sees where stdout actually ended up.
        let stdin_redirects: Vec<Redirect> = process
            .redirects()
            .iter()
            .filter(|redirect| redirect.is_stdin())
            .cloned()
            .collect();
        let applied_stdin = redirect::apply(&stdin_redirects, ctx)?;
        let input_fd = applied_stdin.changed_stdin().then_some(ctx.infile);

        // Use launch for automatic capture (modified internal logic)
        let (pid, mut next_process, applied_output) =
            match process.launch(ctx, shell, self.stdout, pty).await {
                Ok(launched) => launched,
                Err(err) => {
                    // The guard is about to drop and close the input file, so
                    // put `ctx` back first rather than leaving it naming a
                    // descriptor that no longer exists.
                    applied_stdin.restore(ctx);
                    ctx.infile = previous_infile;
                    return Err(err);
                }
            };
        if self.pid.is_none() {
            self.pid = Some(pid); // set process pid
        }
        self.state = process.get_state();

        if ctx.interactive {
            if self.pgid.is_none() {
                self.pgid = Some(pid);
                ctx.pgid = Some(pid);
                debug!("set job id: {} pgid: {:?}", self.id, self.pgid);
            }

            // Full-proxy PTY jobs create a new session in the child, so the
            // parent must not make them process-group leaders first.
            if pty.is_none_or(|pty| pty.mode == PtyMode::OutputOnly) {
                debug!("🔧 PGID: Setting process group for {}", process.get_cmd());
                debug!(
                    "🔧 PGID: setpgid {} pid:{} pgid:{:?}",
                    process.get_cmd(),
                    pid,
                    self.pgid
                );

                let target_pgid = self.pgid.unwrap_or(pid);
                debug!("🔧 PGID: Target pgid: {}", target_pgid);

                match setpgid(pid, target_pgid) {
                    Ok(_) => debug!(
                        "🔧 PGID: Successfully set pgid {} for pid {}",
                        target_pgid, pid
                    ),
                    Err(e) => {
                        let tolerated_output_only_race = if self.pty_mode
                            == Some(PtyMode::OutputOnly)
                        {
                            let already_in_group =
                                getpgid(Some(pid)).is_ok_and(|pgid| pgid == target_pgid);
                            debug!(
                                "🔧 PGID: setpgid failed for output-only PTY job (pid {}, pgid {}, already_in_group={}): {}",
                                pid, target_pgid, already_in_group, e
                            );
                            already_in_group
                        } else {
                            false
                        };
                        if !tolerated_output_only_race {
                            error!(
                                "🔧 PGID: Failed to set pgid {} for pid {}: {}",
                                target_pgid, pid, e
                            );
                            return Err(e.into());
                        }
                    }
                }
            } else {
                debug!(
                    "Skipping parent setpgid for full-proxy PTY job (child {} will setsid)",
                    pid
                );
            }
        }

        let (stdout, stderr) = process.get_cap_out();
        if let Some(stdout) = stdout {
            let monitor = OutputMonitor::new(
                stdout,
                ctx.output_observer.clone(),
                dsh_types::observed_output::ObservedStream::Stdout,
            );
            self.monitors.push(monitor);
        }

        if let Some(stderr) = stderr {
            let monitor = OutputMonitor::new(
                stderr,
                ctx.output_observer.clone(),
                dsh_types::observed_output::ObservedStream::Stderr,
            );
            self.monitors.push(monitor);
        }

        let (stdin, stdout, stderr) = process.get_io();
        let pty_slave = pty.map(|pty| pty.slave);
        if stdin != self.stdin {
            let should_close = match input_fd {
                Some(fd) => stdin != fd,
                None => pty_slave != Some(stdin), // Don't close if it's pty_slave
            } && !applied_stdin.owns(stdin);
            if should_close && let Err(e) = close(stdin) {
                debug!("failed close stdin: {}", e);
                // Don't error out here, just log (avoid crash if EBADF)
            }
        }
        // A `< file` on a pipeline stage takes over the stdin slot, so the read
        // end of the previous stage's pipe never reaches the check above and
        // nobody closed it. The writer then never saw EOF: `yes | wc -l < f`
        // left `yes` blocked forever with the shell still holding the pipe.
        if applied_stdin.changed_stdin()
            && previous_infile != self.stdin
            && previous_infile != ctx.infile
            && pty_slave != Some(previous_infile)
            && !applied_stdin.owns(previous_infile)
            && let Err(e) = close(previous_infile)
        {
            debug!("failed close inherited pipe read end: {}", e);
        }
        // A redirection's file is owned by `applied_output` and closed when it
        // drops; closing it here as well would be a double close. `captured_out`
        // is the caller's pipe (`execute_with_capture`, `$( )`), which the
        // caller closes once every stage has been launched.
        if stdout != self.stdout
            && Some(stdout) != ctx.captured_out
            && pty_slave != Some(stdout)
            && !applied_output.owns(stdout)
            && let Err(e) = close(stdout)
        {
            debug!("failed close stdout: {}", e);
        }
        if stderr != self.stderr
            && stdout != stderr
            && pty_slave != Some(stderr)
            && !applied_output.owns(stderr)
            && let Err(e) = close(stderr)
        {
            debug!("failed close stderr: {}", e);
        }

        // Release the redirection files now. The child inherited them at fork,
        // and a duplicate of a pipe's write end kept here would stop the next
        // command in the pipeline from ever seeing EOF.
        drop(applied_output);
        drop(applied_stdin);

        self.set_process(process.to_owned());
        self.show_job_status();

        if let Some(fd) = input_fd
            && ctx.infile == fd
        {
            ctx.infile = previous_infile;
        }

        // run next pipeline process
        if let Some(mut next_process) = next_process.take()
            && let Err(err) =
                Box::pin(self.launch_process(ctx, shell, &mut next_process, pty)).await
        {
            debug!("err {:?}", err);
            return Err(err);
        }

        Ok(())
    }

    pub async fn put_in_foreground(&mut self, no_hang: bool, cont: bool) -> Result<()> {
        job_wait::put_in_foreground(self, no_hang, cont).await
    }

    /// Synchronous version of put_in_foreground for use in non-async contexts
    /// This method uses spawn_blocking to handle the async operations safely
    pub fn put_in_foreground_sync(&mut self, no_hang: bool, cont: bool) -> Result<()> {
        job_wait::put_in_foreground_sync(self, no_hang, cont)
    }

    pub async fn put_in_background(&mut self) -> Result<()> {
        job_wait::put_in_background(self).await
    }

    fn show_job_status(&self) {}

    pub async fn wait_job(&mut self, no_hang: bool) -> Result<()> {
        job_wait::wait_job(self, no_hang).await
    }

    /// Synchronous version of wait_job for use in non-async contexts
    pub fn wait_job_sync(&mut self, no_hang: bool) -> Result<()> {
        job_wait::wait_job_sync(self, no_hang)
    }

    pub(crate) fn set_process_state(&mut self, pid: Pid, state: ProcessState) {
        if let Some(process) = self.process.as_mut() {
            process.set_state_pid(pid, state);
        }
    }

    pub async fn check_background_output(&mut self) -> Result<()> {
        job_wait::check_background_output(self).await
    }

    pub async fn check_background_all_output(&mut self) -> Result<()> {
        job_wait::check_background_all_output(self).await
    }

    pub fn kill(&mut self) -> Result<()> {
        use super::signal::kill_process;
        kill_process(&self.process)
    }

    pub fn update_status(&mut self) -> bool {
        let old_state = self.state;

        if let Some(process) = self.process.as_mut()
            && let Some(state) = process.update_state()
        {
            self.state = state;

            if old_state != self.state {
                debug!(
                    "JOB_STATE_CHANGE: Job {} state changed: {:?} -> {:?} (pid: {:?}, pgid: {:?})",
                    self.job_id, old_state, self.state, self.pid, self.pgid
                );

                match (&old_state, &self.state) {
                    (ProcessState::Running, ProcessState::Stopped(pid, signal)) => {
                        debug!(
                            "JOB_STOPPED: Job {} stopped by signal {:?} (pid: {:?})",
                            self.job_id, signal, pid
                        );
                    }
                    (ProcessState::Stopped(_, _), ProcessState::Running) => {
                        debug!(
                            "JOB_RESUMED: Job {} resumed from stopped state",
                            self.job_id
                        );
                    }
                    (ProcessState::Running, ProcessState::Completed(exit_code, signal)) => {
                        debug!(
                            "JOB_COMPLETED: Job {} completed with exit_code: {}, signal: {:?}",
                            self.job_id, exit_code, signal
                        );
                    }
                    (ProcessState::Stopped(_, _), ProcessState::Completed(exit_code, signal)) => {
                        debug!(
                            "JOB_COMPLETED_FROM_STOP: Job {} completed from stopped state with exit_code: {}, signal: {:?}",
                            self.job_id, exit_code, signal
                        );
                    }
                    _ => {
                        debug!(
                            "JOB_STATE_OTHER: Job {} other state transition: {:?} -> {:?}",
                            self.job_id, old_state, self.state
                        );
                    }
                }
            }
        }

        let is_completed = is_job_completed(self);
        debug!(
            "JOB_COMPLETION_CHECK: Job {} completion check result: {} (current state: {:?})",
            self.job_id, is_completed, self.state
        );

        is_completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::wait::is_job_completed;
    use crate::shell::SHELL_TERMINAL;
    use nix::sys::termios::tcgetattr;
    use nix::unistd::{Pid, getpgrp, getpid, isatty};
    use std::os::fd::BorrowedFd;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_find_job() {
        init();
        let pgid1 = Pid::from_raw(1);
        let pgid2 = Pid::from_raw(2);
        let pgid3 = Pid::from_raw(3);

        let mut job1 = Job::new_with_process("test1".to_owned(), "".to_owned(), vec![]);
        job1.pgid = Some(pgid1);
        let mut job2 = Job::new_with_process("test2".to_owned(), "".to_owned(), vec![]);
        job2.pgid = Some(pgid2);
        let mut job3 = Job::new_with_process("test3".to_owned(), "".to_owned(), vec![]);
        job3.pgid = Some(pgid3);
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    fn pipeline_of(commands: &[Vec<String>]) -> Job {
        let mut job = Job::new("test".to_string(), Pid::from_raw(1));
        for command in commands {
            let process = Process::new(command[0].clone(), command.clone());
            job.set_process(JobProcess::Command(process));
        }
        job
    }

    #[test]
    fn schema_args_go_to_the_last_external_command() {
        let mut job = pipeline_of(&[argv(&["ps", "aux"]), argv(&["grep", "dsh"])]);
        assert_eq!(job.last_external_argv(), Some(argv(&["grep", "dsh"])));

        job.append_args_to_last_external(&argv(&["--color=never"]));
        assert_eq!(
            job.last_external_argv(),
            Some(argv(&["grep", "dsh", "--color=never"]))
        );
    }

    #[test]
    fn schema_args_are_inserted_before_a_pathspec_terminator() {
        // Appending after `--` would turn the injected flags into pathspecs
        // and break the command.
        let mut job = pipeline_of(&[argv(&["git", "log", "--", "README.md"])]);
        job.append_args_to_last_external(&argv(&["--pretty=format:%h", "--date=short"]));
        assert_eq!(
            job.last_external_argv(),
            Some(argv(&[
                "git",
                "log",
                "--pretty=format:%h",
                "--date=short",
                "--",
                "README.md"
            ]))
        );

        // A command literally named `--` (argv[0]) is not a terminator.
        let mut job = pipeline_of(&[argv(&["--", "x"])]);
        job.append_args_to_last_external(&argv(&["-o"]));
        assert_eq!(job.last_external_argv(), Some(argv(&["--", "x", "-o"])));
    }

    #[test]
    #[ignore] // Ignore this test as it requires a TTY environment
    fn create_job() -> Result<()> {
        init();
        let input = "/usr/bin/touch".to_string();
        let _path = input.clone();
        let _argv: Vec<String> = input.split_whitespace().map(|s| s.to_string()).collect();
        let job = &mut Job::new(input, getpgrp());

        let process = Process::new("1".to_string(), vec![]);
        job.set_process(JobProcess::Command(process));
        let process = Process::new("2".to_string(), vec![]);
        job.set_process(JobProcess::Command(process));

        let pid = getpid();
        let pgid = getpgrp();

        // Skip TTY-dependent operations in test environment
        if isatty(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }).unwrap_or(false) {
            let tmode = match tcgetattr(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }) {
                Ok(mode) => mode,
                Err(_) => return Ok(()),
            };
            let _ctx = Context::new(pid, pgid, Some(tmode), true);
        } else {
            // Create a mock context for non-TTY environments
            println!("Skipping TTY-dependent test operations");
        }

        Ok(())
    }

    #[test]
    fn test_job_completion_with_consumer_termination() {
        init();

        let shell_pgid = getpgrp();
        let mut job = Job::new("cat file | less".to_string(), shell_pgid);

        // Create pipeline processes
        let mut cat_process = Process::new("cat".to_string(), vec!["cat".to_string()]);
        let mut less_process = Process::new("less".to_string(), vec!["less".to_string()]);

        // Set states: cat running, less completed normally
        cat_process.state = ProcessState::Running;
        less_process.state = ProcessState::Completed(0, None);

        // Link pipeline
        cat_process.next = Some(Box::new(JobProcess::Command(less_process)));
        job.set_process(JobProcess::Command(cat_process));

        // Job should be considered completed due to consumer termination
        assert!(is_job_completed(&job));
    }

    #[test]
    fn test_normal_pipeline_completion() {
        init();

        let shell_pgid = getpgrp();
        let mut job = Job::new("cat file | less".to_string(), shell_pgid);

        // Create pipeline processes
        let mut cat_process = Process::new("cat".to_string(), vec!["cat".to_string()]);
        let mut less_process = Process::new("less".to_string(), vec!["less".to_string()]);

        // Set states: both completed
        cat_process.state = ProcessState::Completed(0, None);
        less_process.state = ProcessState::Completed(0, None);

        // Link pipeline
        cat_process.next = Some(Box::new(JobProcess::Command(less_process)));
        job.set_process(JobProcess::Command(cat_process));

        // Job should be completed normally
        assert!(is_job_completed(&job));
    }
}
