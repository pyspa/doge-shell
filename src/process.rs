use crate::builtin::BuiltinCommand;
use crate::shell::{Shell, SHELL_TERMINAL};
use anyhow::Context as _;
use anyhow::Result;
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use log::{debug, error};
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::termios::Termios;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{close, dup2, execv, fork, getpid, pipe, setpgid, tcsetpgrp, ForkResult, Pid};
use std::ffi::CString;
use std::os::unix::io::RawFd;

fn copy_fd(src: RawFd, dst: RawFd) {
    if src != dst {
        dup2(src, dst).expect("failed dup2");
        close(src).expect("failed close");
    }
}

#[derive(Debug)]
pub struct WaitJob {
    pub job_id: usize,
    pub pid: Pid,
    pub cmd: String,
}

pub struct Context {
    pub shell_pid: Pid,
    pub shell_pgid: Pid,
    pub shell_tmode: Termios,
    pub foreground: bool,
    pub interactive: bool,
    pub infile: RawFd,
    pub outfile: RawFd,
    pub errfile: RawFd,
}

impl Context {
    pub fn new(shell_pid: Pid, shell_pgid: Pid, shell_tmode: Termios, foreground: bool) -> Self {
        Context {
            shell_pid,
            shell_pgid,
            shell_tmode,
            foreground,
            interactive: true,
            infile: STDIN_FILENO,
            outfile: STDOUT_FILENO,
            errfile: STDERR_FILENO,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum JobProcess {
    Builtin(BuiltinProcess),
    Command(Process),
}

impl std::fmt::Debug for JobProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        match self {
            JobProcess::Builtin(jprocess) => f
                .debug_struct("JobProcess::Builtin")
                .field("builtin", jprocess)
                .finish(),
            JobProcess::Command(jprocess) => f
                .debug_struct("JobProcess::Command")
                .field("command", jprocess)
                .finish(),
        }
    }
}

impl JobProcess {
    pub fn link(&mut self, process: JobProcess) {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.link(process),
            JobProcess::Command(jprocess) => jprocess.link(process),
        }
    }

    pub fn next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn set_io(&mut self, stdin: RawFd, stdout: RawFd) {
        match self {
            JobProcess::Builtin(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
            }
            JobProcess::Command(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
            }
        }
    }

    pub fn get_io(&self) -> (RawFd, RawFd) {
        match self {
            JobProcess::Builtin(jprocess) => (jprocess.stdin, jprocess.stdout),
            JobProcess::Command(jprocess) => (jprocess.stdin, jprocess.stdout),
        }
    }

    pub fn set_pid(&mut self, pid: Option<Pid>) {
        match self {
            JobProcess::Builtin(_) => {
                // noop
            }
            JobProcess::Command(process) => {
                process.pid = pid;
            }
        }
    }

    pub fn get_pid(&self) -> Option<Pid> {
        match self {
            JobProcess::Builtin(_) => {
                // noop
                None
            }
            JobProcess::Command(process) => process.pid,
        }
    }

    pub fn set_state(&mut self, state: ProcessState) {
        match self {
            JobProcess::Builtin(p) => p.state = state,
            JobProcess::Command(p) => p.state = state,
        }
    }

    pub fn get_state(&self) -> ProcessState {
        match self {
            JobProcess::Builtin(p) => p.state,
            JobProcess::Command(p) => p.state,
        }
    }

    fn is_stopped(&self) -> bool {
        if self.get_state() == ProcessState::Running {
            return false;
        }
        if let Some(p) = self.next() {
            return p.is_stopped();
        }
        true
    }

    fn is_completed(&self) -> bool {
        match self.get_state() {
            ProcessState::Completed(_) => {
                //ok
                if let Some(p) = self.next() {
                    return p.is_completed();
                }
            }
            _ => {
                return false;
            }
        };
        true
    }
}

#[derive(Clone)]
pub struct BuiltinProcess {
    cmd_fn: BuiltinCommand,
    argv: Vec<String>,
    state: ProcessState, // completed, stopped,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
}

impl PartialEq for BuiltinProcess {
    fn eq(&self, other: &Self) -> bool {
        self.argv == other.argv
    }
}

impl Eq for BuiltinProcess {}

impl std::fmt::Debug for BuiltinProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("BuiltinProcess")
            .field("argv", &self.argv)
            .field("state", &self.state)
            .field("next", &self.next)
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

impl BuiltinProcess {
    pub fn new(cmd_fn: BuiltinCommand, argv: Vec<String>) -> Self {
        BuiltinProcess {
            cmd_fn,
            argv,
            state: ProcessState::Running,
            next: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
        }
    }

    pub fn link(&mut self, process: JobProcess) {
        match self.next {
            Some(ref mut p) => {
                p.link(process);
            }
            None => {
                self.next = Some(Box::new(process));
            }
        }
    }

    pub fn launch(&mut self, ctx: &mut Context, shell: &mut Shell) -> Result<()> {
        debug!(
            "launch: builtin process infile:{:?} outfile:{:?}",
            self.stdin, self.stdout
        );
        let _exit = (self.cmd_fn)(ctx, self.argv.to_vec(), shell);
        // TODO check exit
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    cmd: String,
    argv: Vec<String>,
    pid: Option<Pid>,
    status: Option<ExitStatus>,
    state: ProcessState, // completed, stopped,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProcessState {
    Running,
    Completed(i32),
    Stopped(Pid),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ExitStatus {
    ExitedWith(i32),
    Running(Pid),
    Break,
    Continue,
    Return,
}

impl Process {
    pub fn new(cmd: String, argv: Vec<String>) -> Self {
        Process {
            cmd,
            argv,
            pid: None,
            status: None,
            state: ProcessState::Running,
            next: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
        }
    }

    pub fn link(&mut self, process: JobProcess) {
        match self.next {
            Some(ref mut p) => {
                p.link(process);
            }
            None => {
                self.next = Some(Box::new(process));
            }
        }
    }

    fn set_signals(&self) {
        // Accept job-control-related signals (refer https://www.gnu.org/software/libc/manual/html_node/Launching-Jobs.html)
        let action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        unsafe {
            sigaction(Signal::SIGINT, &action).expect("failed to sigaction");
            sigaction(Signal::SIGQUIT, &action).expect("failed to sigaction");
            sigaction(Signal::SIGTSTP, &action).expect("failed to sigaction");
            sigaction(Signal::SIGTTIN, &action).expect("failed to sigaction");
            sigaction(Signal::SIGTTOU, &action).expect("failed to sigaction");
            sigaction(Signal::SIGCHLD, &action).expect("failed to sigaction");
        }
    }

    pub fn launch(&mut self, pgid: Option<Pid>, foreground: bool) -> Result<()> {
        let pid = getpid();
        debug!("launch: foreground:{:?}", foreground);
        debug!(
            "launch: child process setpgid pid:{:?} pgid:{:?}",
            pid, pgid
        );

        let pgid = pgid.unwrap_or(pid);
        setpgid(pid, pgid).context("failed setpgid")?;

        if foreground {
            tcsetpgrp(SHELL_TERMINAL, pgid).context("failed tcsetpgrp")?;
            debug!("launch: tcsetpgrp pgid");
        }

        self.set_signals();

        debug!(
            "launch: process {:?} infile:{:?} outfile:{:?}",
            self.cmd, self.stdin, self.stdout
        );

        copy_fd(self.stdin, STDIN_FILENO);
        copy_fd(self.stdout, STDOUT_FILENO);
        copy_fd(self.stderr, STDERR_FILENO);

        let cmd = CString::new(self.cmd.clone()).context("failed new CString")?;
        let argv: Vec<CString> = self
            .argv
            .clone()
            .into_iter()
            .filter(|a| !a.is_empty())
            .map(|a| CString::new(a).expect("failed new CString"))
            .collect();

        debug!("launch: execv cmd:{:?} argv:{:?}", cmd, argv);
        match execv(&cmd, &argv) {
            Ok(_) => {
                unreachable!();
            }
            Err(nix::errno::Errno::EACCES) => {
                error!("Failed to exec {:?} (EACCESS). chmod(1) may help.", cmd);
                std::process::exit(1);
            }
            Err(err) => {
                error!("Failed to exec {:?} ({})", cmd, err);
                std::process::exit(1);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub cmd: String,
    pub pgid: Option<Pid>,
    pub process: Option<Box<JobProcess>>,
    notified: bool,
    tmodes: Option<Termios>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
    pub next: Option<Box<Job>>,
    pub foreground: bool,
}

impl Job {
    pub fn new_with_process(cmd: String, path: String, argv: Vec<String>) -> Self {
        let process = JobProcess::Command(Process::new(path, argv));
        Job {
            cmd,
            pgid: None,
            process: Some(Box::new(process)),
            notified: false,
            tmodes: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            next: None,
            foreground: true,
        }
    }

    pub fn new(cmd: String) -> Self {
        Job {
            cmd,
            pgid: None,
            process: None,
            notified: false,
            tmodes: None,
            stdin: STDIN_FILENO,
            stdout: STDOUT_FILENO,
            stderr: STDERR_FILENO,
            next: None,
            foreground: true,
        }
    }

    pub fn link(&mut self, job: Job) {
        match self.next {
            Some(ref mut j) => {
                j.link(job);
            }
            None => {
                self.next = Some(Box::new(job));
            }
        }
    }

    pub fn set_process(&mut self, process: JobProcess) {
        match self.process {
            Some(ref mut p) => p.link(process),
            None => self.process = Some(Box::new(process)),
        }
    }

    pub fn launch(&mut self, ctx: &mut Context, shell: &mut Shell) -> Result<()> {
        ctx.foreground = self.foreground;
        self.process
            .take()
            .as_mut()
            .map(|process| self.launch_process(ctx, shell, process));

        Ok(())
    }

    pub fn launch_process(
        &mut self,
        ctx: &mut Context,
        shell: &mut Shell,
        process: &mut JobProcess,
    ) -> Result<()> {
        let mut pipe_out = None;

        match process.next() {
            Some(_) => {
                let (pout, pin) = pipe().context("failed pipe")?;
                ctx.outfile = pin;
                pipe_out = Some(pout);
            }
            _ => {
                ctx.outfile = self.stdout;
            }
        }

        process.set_io(ctx.infile, ctx.outfile);

        let pid = match process {
            JobProcess::Builtin(process) => {
                let pid = getpid();
                process.launch(ctx, shell)?;
                pid
            }
            JobProcess::Command(process) => {
                // fork
                fork_process(ctx, self.pgid, process)?
            }
        };

        if ctx.interactive {
            if self.pgid.is_none() {
                self.pgid = Some(pid);
                debug!("job pgid {:?}", self.pgid);
            }
            // debug!("parent setpgid pid:{:?} pgid:{:?}", pid, self.pgid);
            setpgid(pid, self.pgid.unwrap_or(pid))?;
        }

        process.set_pid(Some(pid));
        if !ctx.foreground {
            let job_id = if shell.wait_jobs.is_empty() {
                1
            } else if let Some(wait) = shell.wait_jobs.last() {
                wait.job_id + 1
            } else {
                1
            };

            if process.next().is_none() {
                // background
                shell.wait_jobs.push(WaitJob {
                    job_id,
                    pid,
                    cmd: self.cmd.clone(),
                });
            }
        }

        self.show_job_status();

        if let Some(pipe_out) = pipe_out {
            ctx.infile = pipe_out;
        }
        let (stdin, stdout) = process.get_io();
        if stdin != self.stdin {
            close(stdin).context("failed close")?;
        }
        if stdout != self.stdout {
            close(stdout).context("failed close")?;
        }

        let mut next_process = process.next().take();
        self.set_process(process.to_owned());

        if let Some(Err(err)) = next_process
            .take()
            .as_mut()
            .map(|process| self.launch_process(ctx, shell, process))
        {
            return Err(err);
        }

        if !ctx.interactive {
            self.wait_job();
            Ok(())
        } else if ctx.foreground {
            // foreground
            self.put_in_foreground(ctx)
        } else {
            // background
            self.put_in_background(ctx)
        }
    }

    fn put_in_foreground(&mut self, ctx: &Context) -> Result<()> {
        debug!("put_in_foreground: pgid {:?}", self.pgid);
        // Put the job into the foreground

        tcsetpgrp(SHELL_TERMINAL, self.pgid.unwrap()).context("failed tcsetpgrp")?;
        debug!("put_in_foreground: tcsetpgrp pgid");

        // TODO Send the job a continue signal, if necessary.

        self.wait_job();

        tcsetpgrp(SHELL_TERMINAL, ctx.shell_pgid).context("failed tcsetpgrp shell_pgid")?;
        debug!("put_in_foreground: tcsetpgrp shell");

        // let tmodes = tcgetattr(SHELL_TERMINAL).context("failed tcgetattr wait")?;
        // self.tmodes = Some(tmodes);

        // let _ = tcsetattr(SHELL_TERMINAL, TCSADRAIN, &ctx.shell_tmode)
        //     .context("failed tcsetattr restore shell_mode")?;

        Ok(())
    }

    fn put_in_background(&mut self, _ctx: &Context) -> Result<()> {
        debug!("put_in_background pgid {:?}", self.pgid);

        // TODO Send the job a continue signal, if necessary.

        // let _ = tcsetpgrp(SHELL_TERMINAL, ctx.shell_pgid).context("failed tcsetpgrp shell_pgid")?;
        // let tmodes = tcgetattr(SHELL_TERMINAL).context("failed tcgetattr wait")?;
        // self.tmodes = Some(tmodes);

        Ok(())
    }

    fn show_job_status(&self) {}

    pub fn wait_job(&mut self) {
        loop {
            // TODO other process waitpid
            let result = waitpid(None, Some(WaitPidFlag::WUNTRACED));

            let (pid, state) = match result {
                Ok(WaitStatus::Exited(pid, status)) => (pid, ProcessState::Completed(status)),
                Ok(WaitStatus::Signaled(pid, _signal, _)) => (pid, ProcessState::Completed(-1)),
                Ok(WaitStatus::Stopped(pid, _signal)) => (pid, ProcessState::Stopped(pid)),
                Err(nix::errno::Errno::ECHILD) | Ok(WaitStatus::StillAlive) => {
                    // break?
                    return;
                }
                status => {
                    panic!("unexpected waitpid event: {:?}", status);
                }
            };
            debug!("wait_job: waitpid result {:?} {:?}", pid, state);

            self.set_process_state(pid, state);

            debug!(
                "wait_job: complete:{:?} stopped:{:?}",
                is_job_completed(self),
                is_job_stopped(self),
            );

            if is_job_completed(self) || is_job_stopped(self) {
                break;
            }
        }
    }

    fn set_process_state(&mut self, pid: Pid, state: ProcessState) {
        if let Some(process) = self.process.as_mut() {
            debug!("set_process_state: pid:{:?}", pid);
            set_process_state(process, pid, state)
        }
    }
}

pub fn wait_any_job(no_block: bool) -> Option<(Pid, ProcessState)> {
    let options = if no_block {
        WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG
    } else {
        WaitPidFlag::WUNTRACED
    };

    let result = waitpid(None, Some(options));
    let res = match result {
        Ok(WaitStatus::Exited(pid, status)) => (pid, ProcessState::Completed(status)),
        Ok(WaitStatus::Signaled(pid, _signal, _)) => (pid, ProcessState::Completed(-1)),
        Ok(WaitStatus::Stopped(pid, _signal)) => (pid, ProcessState::Stopped(pid)),
        Err(nix::errno::Errno::ECHILD) | Ok(WaitStatus::StillAlive) => {
            return None;
        }
        status => {
            panic!("unexpected waitpid event: {:?}", status);
        }
    };
    Some(res)
}

fn set_process_state(process: &mut JobProcess, pid: Pid, state: ProcessState) {
    if let Some(ppid) = process.get_pid() {
        if ppid == pid {
            debug!("set_process_state: found pid:{:?}", pid);
            process.set_state(state);
        }
    }
}

fn fork_process(ctx: &Context, job_pgid: Option<Pid>, process: &mut Process) -> Result<Pid> {
    let pid = unsafe { fork().context("failed fork")? };
    match pid {
        ForkResult::Parent { child } => {
            process.pid = Some(child);
            Ok(child)
        }
        ForkResult::Child => {
            // This is the child process
            process.launch(job_pgid, ctx.foreground)?;
            unreachable!();
        }
    }
}

pub fn find_job(first_job: &Job, pgid: Pid) -> Option<Job> {
    let mut job = first_job;
    while let Some(ref bj) = job.next {
        if bj.pgid == Some(pgid) {
            let j = *bj.clone();
            return Some(j);
        }
        job = bj;
    }
    None
}

pub fn is_job_stopped(job: &Job) -> bool {
    debug!("is_job_stopped: process:{:?}", job);
    if let Some(p) = &job.process {
        p.is_stopped()
    } else {
        true
    }
}

pub fn is_job_completed(job: &Job) -> bool {
    debug!("is_job_completed: process:{:?}", job);
    if let Some(p) = &job.process {
        p.is_completed()
    } else {
        true
    }
}

#[cfg(test)]
mod test {

    use nix::sys::termios::tcgetattr;
    use nix::unistd::getpgrp;

    use super::*;

    #[test]
    fn init() {
        let _ = env_logger::try_init();
    }

    #[test]
    fn test_find_job() {
        let pgid1 = Pid::from_raw(1);
        let pgid2 = Pid::from_raw(2);
        let pgid3 = Pid::from_raw(3);

        let mut job1 = Job::new_with_process("test1".to_owned(), "".to_owned(), vec![]);
        job1.pgid = Some(pgid1);
        let mut job2 = Job::new_with_process("test2".to_owned(), "".to_owned(), vec![]);
        job2.pgid = Some(pgid2);
        let mut job3 = Job::new_with_process("test3".to_owned(), "".to_owned(), vec![]);
        job3.pgid = Some(pgid3);

        job2.link(job3);
        job1.link(job2);

        let found = find_job(&job1, pgid2).unwrap();
        assert_eq!(found.pgid.unwrap().as_raw(), pgid2.as_raw());

        let pgid4 = Pid::from_raw(4);
        let nt = find_job(&job1, pgid4);
        assert_eq!(nt, None::<Job>);
    }

    #[test]
    fn create_job() -> Result<()> {
        let input = "/usr/bin/touch".to_string();
        let _path = input.clone();
        let _argv: Vec<String> = input.split_whitespace().map(|s| s.to_string()).collect();
        let job = &mut Job::new(input);

        let process = Process::new("1".to_string(), vec![]);
        job.set_process(JobProcess::Command(process));
        let process = Process::new("2".to_string(), vec![]);
        job.set_process(JobProcess::Command(process));

        let pid = getpid();
        let pgid = getpgrp();
        let tmode = tcgetattr(SHELL_TERMINAL).expect("failed cgetattr");
        let _ctx = Context::new(pid, pgid, tmode, true);

        // info!("launch");

        Ok(())
    }

    #[test]
    fn is_stopped() {
        let _ = env_logger::try_init();

        let input = "/usr/bin/touch";

        let job = &mut Job::new(input.to_string());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let process = Process::new("3".to_string(), vec![]);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert_eq!(false, is_job_stopped(job));

        let job = &mut Job::new(input.to_string());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Stopped(Pid::from_raw(10));
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert_eq!(true, is_job_stopped(job));
    }

    #[test]
    fn is_completed() {
        let _ = env_logger::try_init();

        let input = "/usr/bin/touch";

        let job = &mut Job::new(input.to_string());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Stopped(Pid::from_raw(0));
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert_eq!(false, is_job_completed(job));

        let job = &mut Job::new(input.to_string());
        let mut process = Process::new("1".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("2".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        let mut process = Process::new("3".to_string(), vec![]);
        process.state = ProcessState::Completed(0);
        job.set_process(JobProcess::Command(process));

        debug!("{:?}", job);
        assert_eq!(true, is_job_completed(job));
    }
}
