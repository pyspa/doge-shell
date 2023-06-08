use crate::shell::{Shell, SHELL_TERMINAL};
use anyhow::Context as _;
use anyhow::Result;
use async_std::{fs, io, task};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use dsh_builtin::BuiltinCommand;
use dsh_types::{Context, ExitStatus};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::termios::Termios;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{close, dup2, execv, fork, getpid, pipe, setpgid, tcsetpgrp, ForkResult, Pid};
use std::ffi::CString;
use std::fmt::Debug;
use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;
use std::os::unix::io::RawFd;
use tracing::{debug, error};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redirect {
    StdoutOutput(String),
    StdoutAppend(String),
    StderrOutput(String),
    StderrAppend(String),
    StdouterrOutput(String),
    StdouterrAppend(String),
    Input,
}

impl Redirect {
    fn process(&self, ctx: &mut Context) {
        match self {
            Redirect::StdoutOutput(out)
            | Redirect::StderrOutput(out)
            | Redirect::StdouterrOutput(out) => {
                let infile = ctx.infile;
                let file = out.to_string();
                // spawn and io copy
                task::spawn(async move {
                    // copy io
                    let mut reader = unsafe { fs::File::from_raw_fd(infile) };
                    let mut writer = fs::File::create(file.to_string()).await.unwrap(); // TODO check err
                    let _res = io::copy(&mut reader, &mut writer).await; // TODO check err
                });
            }

            Redirect::StdoutAppend(out)
            | Redirect::StderrAppend(out)
            | Redirect::StdouterrAppend(out) => {
                let infile = ctx.infile;
                let file = out.to_string();
                // spawn and io copy
                task::spawn(async move {
                    // copy io
                    let mut reader = unsafe { fs::File::from_raw_fd(infile) };
                    let mut writer = fs::OpenOptions::new()
                        .write(true)
                        .append(true)
                        .open(file.to_string())
                        .await
                        .unwrap();
                    let _res = io::copy(&mut reader, &mut writer).await; // TODO check err
                });
            }
            Redirect::Input => {}
        }
    }
}

fn copy_fd(src: RawFd, dst: RawFd) {
    if src != dst {
        dup2(src, dst).expect("failed dup2");
        close(src).expect("failed close");
    }
}

fn get_job_id(shell: &Shell) -> usize {
    if shell.wait_jobs.is_empty() {
        1
    } else if let Some(wait) = shell.wait_jobs.last() {
        wait.wait_job_id + 1
    } else {
        1
    }
}

#[derive(Debug)]
pub struct WaitJob {
    pub job_id: String,
    pub wait_job_id: usize,
    pub pid: Pid,
    pub cmd: String,
    pub stdout: Option<RawFd>,
    pub stderr: Option<RawFd>,
    pub foreground: bool,
    pub state: ProcessState,
}

impl WaitJob {
    pub fn new(
        job: &Job,
        job_process: &JobProcess,
        shell: &Shell,
        pid: Pid,
        foreground: bool,
    ) -> Self {
        let (stdout, stderr) = job_process.get_cap_out();
        WaitJob {
            job_id: job.id.clone(),
            wait_job_id: get_job_id(shell),
            pid,
            cmd: job.cmd.clone(),
            stdout,
            stderr,
            foreground,
            state: ProcessState::Running,
        }
    }

    pub fn output(&self) {
        if let Some(fd) = self.stdout {
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            let mut buf = String::new();
            let mut out = std::io::stdout().lock();

            match file.read_to_string(&mut buf) {
                Ok(size) => {
                    if size > 0 {
                        disable_raw_mode().ok();
                        out.write(buf.as_bytes()).ok();
                        enable_raw_mode().ok();
                    }
                }

                Err(_err) => {
                    // break;
                }
            }
        }

        // if let Some(fd) = self.stderr {
        //     let mut file = unsafe { File::from_raw_fd(fd) };
        //     let mut buf = String::new();
        //     match file.read_to_string(&mut buf) {
        //         Ok(size) => {
        //             if size > 0 {
        //                 disable_raw_mode().ok();
        //                 eprint!("{}", buf);
        //                 enable_raw_mode().ok();
        //             }
        //         }

        //         Err(_err) => {
        //             // break;
        //         }
        //     }
        // }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListOp {
    None,
    And,
    Or,
}

#[derive(Clone, PartialEq, Eq)]
pub enum JobProcess {
    Builtin(BuiltinProcess),
    Wasm(WasmProcess),
    Command(Process),
}

impl std::fmt::Debug for JobProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        match self {
            JobProcess::Builtin(jprocess) => f
                .debug_struct("JobProcess::Builtin")
                .field("cmd", &jprocess.argv)
                .finish(),
            JobProcess::Wasm(jprocess) => f
                .debug_struct("JobProcess::WASM")
                .field("wasm", jprocess)
                .finish(),
            JobProcess::Command(jprocess) => f
                .debug_struct("JobProcess::Command")
                .field("cmd", &jprocess.cmd)
                .field("argv", &jprocess.argv)
                .field("pid", &jprocess.pid)
                .field("stdin", &jprocess.stdin)
                .field("stdout", &jprocess.stdout)
                .field("stderr", &jprocess.stderr)
                .finish(),
        }
    }
}

impl JobProcess {
    pub fn link(&mut self, process: JobProcess) {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.link(process),
            JobProcess::Wasm(jprocess) => jprocess.link(process),
            JobProcess::Command(jprocess) => jprocess.link(process),
        }
    }

    pub fn next(&self) -> Option<Box<JobProcess>> {
        match self {
            JobProcess::Builtin(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Wasm(jprocess) => jprocess.next.as_ref().cloned(),
            JobProcess::Command(jprocess) => jprocess.next.as_ref().cloned(),
        }
    }

    pub fn set_io(&mut self, stdin: RawFd, stdout: RawFd, stderr: RawFd) {
        match self {
            JobProcess::Builtin(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
            JobProcess::Wasm(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
            JobProcess::Command(jprocess) => {
                jprocess.stdin = stdin;
                jprocess.stdout = stdout;
                jprocess.stderr = stderr;
            }
        }
    }

    pub fn get_io(&self) -> (RawFd, RawFd, RawFd) {
        match self {
            JobProcess::Builtin(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
            JobProcess::Wasm(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
            JobProcess::Command(jprocess) => (jprocess.stdin, jprocess.stdout, jprocess.stderr),
        }
    }

    pub fn set_pid(&mut self, pid: Option<Pid>) {
        match self {
            JobProcess::Builtin(_) => {
                // noop
            }
            JobProcess::Wasm(_) => {
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
            JobProcess::Wasm(_) => {
                // noop
                None
            }
            JobProcess::Command(process) => process.pid,
        }
    }

    pub fn set_state(&mut self, state: ProcessState) {
        match self {
            JobProcess::Builtin(p) => p.state = state,
            JobProcess::Wasm(p) => p.state = state,
            JobProcess::Command(p) => p.state = state,
        }
    }

    pub fn get_state(&self) -> ProcessState {
        match self {
            JobProcess::Builtin(p) => p.state,
            JobProcess::Wasm(p) => p.state,
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

    pub fn get_cap_out(&self) -> (Option<RawFd>, Option<RawFd>) {
        match self {
            JobProcess::Builtin(_p) => (None, None),
            JobProcess::Wasm(_p) => (None, None),
            JobProcess::Command(p) => (p.cap_stdout, p.cap_stderr),
        }
    }

    pub fn get_cmd(&self) -> &str {
        match self {
            JobProcess::Builtin(p) => &p.name,
            JobProcess::Wasm(p) => &p.name,
            JobProcess::Command(p) => &p.cmd,
        }
    }
}

#[derive(Clone)]
pub struct BuiltinProcess {
    name: String,
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
    pub fn new(name: String, cmd_fn: BuiltinCommand, argv: Vec<String>) -> Self {
        BuiltinProcess {
            name,
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
            "launch: builtin process {:?} infile:{:?} outfile:{:?}",
            &self.name, self.stdin, self.stdout
        );
        let exit = (self.cmd_fn)(ctx, self.argv.to_vec(), shell);
        if let ExitStatus::ExitedWith(code) = exit {
            if code >= 0 {
                self.state = ProcessState::Completed(0);
            }
        }
        // TODO check exit
        Ok(())
    }
}

#[derive(Clone)]
pub struct WasmProcess {
    name: String,
    argv: Vec<String>,
    state: ProcessState, // completed, stopped,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
}

impl PartialEq for WasmProcess {
    fn eq(&self, other: &Self) -> bool {
        self.argv == other.argv
    }
}

impl Eq for WasmProcess {}

impl std::fmt::Debug for WasmProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        f.debug_struct("WASMProcess")
            .field("name", &self.name)
            .field("argv", &self.argv)
            .field("state", &self.state)
            .field("next", &self.next)
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

impl WasmProcess {
    pub fn new(name: String, argv: Vec<String>) -> Self {
        WasmProcess {
            name,
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

    pub fn launch(&mut self, _ctx: &mut Context, shell: &mut Shell) -> Result<()> {
        debug!(
            "launch: wasm process infile:{:?} outfile:{:?}",
            self.stdin, self.stdout
        );

        // TODO check exit
        shell.run_wasm(self.name.as_str(), self.argv.to_vec())
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
    cap_stdout: Option<RawFd>,
    cap_stderr: Option<RawFd>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProcessState {
    Running,
    Completed(u8),
    Stopped(Pid),
}

impl std::fmt::Display for ProcessState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ProcessState::Running => formatter.write_str("running"),
            ProcessState::Completed(_) => formatter.write_str("completed"),
            ProcessState::Stopped(_) => formatter.write_str("stopped"),
        }
    }
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
            cap_stdout: None,
            cap_stderr: None,
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

        let pgid = pgid.unwrap_or(pid);
        setpgid(pid, pgid).context("failed setpgid")?;

        if foreground {
            tcsetpgrp(SHELL_TERMINAL, pgid).context("failed tcsetpgrp")?;
        }

        self.set_signals();

        let cmd = CString::new(self.cmd.clone()).context("failed new CString")?;
        let argv: Vec<CString> = self
            .argv
            .clone()
            .into_iter()
            .map(|a| CString::new(a).expect("failed new CString"))
            .collect();

        debug!(
            "launch: execv cmd:{:?} argv:{:?} foreground:{:?} infile:{:?} outfile:{:?} pid:{:?} pgid:{:?}",
            cmd, argv, foreground, self.stdin, self.stdout,pid, pgid,
        );

        copy_fd(self.stdin, STDIN_FILENO);
        if self.stdout == self.stderr {
            dup2(self.stdout, STDOUT_FILENO).expect("failed dup2");
            dup2(self.stderr, STDERR_FILENO).expect("failed dup2");
            close(self.stdout).expect("failed close");
        } else {
            copy_fd(self.stdout, STDOUT_FILENO);
            copy_fd(self.stderr, STDERR_FILENO);
        }
        match execv(&cmd, &argv) {
            Ok(_) => Ok(()),
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
    pub id: String,
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
    pub need_wait: bool,
    pub subshell: bool,
    pub redirect: Option<Redirect>,
    pub list_op: ListOp,
}

impl Job {
    pub fn new_with_process(cmd: String, path: String, argv: Vec<String>) -> Self {
        let process = JobProcess::Command(Process::new(path, argv));
        let id = format!("{}", xid::new());
        Job {
            id,
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
            need_wait: false,
            subshell: false,
            redirect: None,
            list_op: ListOp::None,
        }
    }

    pub fn new(cmd: String) -> Self {
        let id = format!("{}", xid::new());
        Job {
            id,
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
            need_wait: false,
            subshell: false,
            redirect: None,
            list_op: ListOp::None,
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

    pub fn last_process_state(&self) -> ProcessState {
        if let Some(ref p) = &self.process {
            last_process_state(*p.clone())
        } else {
            // not running
            ProcessState::Completed(0)
        }
    }

    pub fn launch(&mut self, ctx: &mut Context, shell: &mut Shell) -> Result<ProcessState> {
        ctx.foreground = self.foreground;

        self.process
            .take()
            .as_mut()
            .map(|process| self.launch_process(ctx, shell, process));

        if ctx.foreground {
            Ok(self.last_process_state())
        } else {
            // background
            Ok(ProcessState::Running)
        }
    }

    pub fn launch_process(
        &mut self,
        ctx: &mut Context,
        shell: &mut Shell,
        process: &mut JobProcess,
    ) -> Result<()> {
        let pipe_out = match process.next() {
            Some(_) => create_pipe(ctx)?,
            None => handle_output_redirect(ctx, &self.redirect, self.stdout)?,
        };

        process.set_io(ctx.infile, ctx.outfile, ctx.errfile);

        debug!("start process ctx {:?}", &ctx);
        debug!("start process {:?}", &process);

        let pid = match process {
            JobProcess::Builtin(process) => {
                let pid = getpid();
                process.launch(ctx, shell)?;
                pid
            }
            JobProcess::Wasm(process) => {
                let pid = getpid();
                process.launch(ctx, shell)?;
                pid
            }
            JobProcess::Command(process) => {
                self.need_wait = true;
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

        if !process.is_completed() {
            let wait_job = WaitJob::new(&self, &process, &shell, pid, ctx.foreground);
            shell.wait_jobs.push(wait_job);
        }

        self.show_job_status();

        if let Some(pipe_out) = pipe_out {
            ctx.infile = pipe_out;
        }
        let (stdin, stdout, stderr) = process.get_io();
        if stdin != self.stdin {
            close(stdin).context("failed close")?;
        }
        if stdout != self.stdout {
            close(stdout).context("failed close stdout")?;
        }
        if stderr != self.stderr && stdout != stderr {
            close(stderr).context("failed close stderr")?;
        }

        let mut next_process = process.next().take();
        self.set_process(process.to_owned());

        if let Some(ref redirect) = self.redirect {
            redirect.process(ctx);
            // // TODO refactor to function
            // let infile = ctx.infile;
            // let file = out.to_string();
            // // spawn and io copy
            // task::spawn(async move {
            //     // copy io
            //     let mut reader = unsafe { fs::File::from_raw_fd(infile) };
            //     // TODO add append mode
            //     let mut writer = fs::File::create(file.to_string()).await.unwrap(); // TODO check err
            //     let _res = io::copy(&mut reader, &mut writer).await; // TODO check err
            // });
        }

        if let Some(Err(err)) = next_process
            .take()
            .as_mut()
            .map(|process| self.launch_process(ctx, shell, process))
        {
            return Err(err);
        }

        if !ctx.interactive {
            self.wait_job(ctx, process, shell);
            Ok(())
        } else if ctx.foreground {
            // foreground

            self.put_in_foreground(ctx, process, shell)
        } else {
            // background
            self.put_in_background(ctx, process, shell)
        }
    }

    fn put_in_foreground(
        &mut self,
        ctx: &Context,
        process: &mut JobProcess,
        shell: &mut Shell,
    ) -> Result<()> {
        debug!(
            "put_in_foreground: {:?} pgid {:?}",
            &process.get_cmd(),
            self.pgid
        );
        // Put the job into the foreground

        tcsetpgrp(SHELL_TERMINAL, self.pgid.unwrap()).context("failed tcsetpgrp")?;
        // debug!("put_in_foreground: {:?} tcsetpgrp pgid", &self.cmd);

        // TODO Send the job a continue signal, if necessary.

        self.wait_job(ctx, process, shell);

        if let Some(pid) = process.get_pid() {
            let mut i = 0;
            while i < shell.wait_jobs.len() {
                if shell.wait_jobs[i].pid == pid {
                    shell.wait_jobs[i].state = process.get_state();
                    debug!("set process: {:?} {:?}", pid, &shell.wait_jobs[i].state);
                    break;
                } else {
                    i += 1;
                }
            }
        }

        tcsetpgrp(SHELL_TERMINAL, ctx.shell_pgid).context("failed tcsetpgrp shell_pgid")?;
        // debug!("put_in_foreground: {:?} tcsetpgrp shell", &self.cmd);

        // let tmodes = tcgetattr(SHELL_TERMINAL).context("failed tcgetattr wait")?;
        // self.tmodes = Some(tmodes);

        // let _ = tcsetattr(SHELL_TERMINAL, TCSADRAIN, &ctx.shell_tmode)
        //     .context("failed tcsetattr restore shell_mode")?;

        Ok(())
    }

    fn put_in_background(
        &mut self,
        _ctx: &Context,
        process: &mut JobProcess,
        shell: &mut Shell,
    ) -> Result<()> {
        debug!(
            "put_in_background {:?} pgid {:?}",
            process.get_cmd(),
            self.pgid,
        );

        // TODO Send the job a continue signal, if necessary.

        // let _ = tcsetpgrp(SHELL_TERMINAL, ctx.shell_pgid).context("failed tcsetpgrp shell_pgid")?;
        // let tmodes = tcgetattr(SHELL_TERMINAL).context("failed tcgetattr wait")?;
        // self.tmodes = Some(tmodes);

        Ok(())
    }

    fn show_job_status(&self) {}

    pub fn wait_job(&mut self, _ctx: &Context, process: &mut JobProcess, shell: &mut Shell) {
        debug!(
            "call wait_job: {:?} pgid: {:?} need_wait: {:?}",
            process.get_cmd(),
            self.pgid,
            self.need_wait
        );

        if !self.need_wait {
            return;
        }

        loop {
            // TODO other process waitpid
            let result = waitpid(None, Some(WaitPidFlag::WUNTRACED));

            let (pid, state) = match result {
                Ok(WaitStatus::Exited(pid, status)) => (pid, ProcessState::Completed(status as u8)), // ok??
                Ok(WaitStatus::Signaled(pid, _signal, _)) => (pid, ProcessState::Completed(1)),
                Ok(WaitStatus::Stopped(pid, _signal)) => (pid, ProcessState::Stopped(pid)),
                Err(nix::errno::Errno::ECHILD) | Ok(WaitStatus::StillAlive) => {
                    // break?
                    return;
                }
                status => {
                    panic!("unexpected waitpid event: {:?}", status);
                }
            };
            self.set_process_state(pid, state);
            set_process_state(process, pid, state);

            debug!(
                "fin wait_job: {:?} pid:{:?} complete:{:?} stopped:{:?}",
                process.get_cmd(),
                pid,
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
            set_process_state(process, pid, state);
        }
    }
}

pub fn wait_any_job(no_hang: bool) -> Option<(Pid, ProcessState)> {
    let options = if no_hang {
        WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG
    } else {
        WaitPidFlag::WUNTRACED
    };

    let result = waitpid(None, Some(options));
    let res = match result {
        Ok(WaitStatus::Exited(pid, status)) => (pid, ProcessState::Completed(status as u8)),
        Ok(WaitStatus::Signaled(pid, _signal, _)) => (pid, ProcessState::Completed(1)),
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

fn last_process_state(process: JobProcess) -> ProcessState {
    if let Some(next_proc) = process.next() {
        last_process_state(*next_proc)
    } else {
        process.get_state()
    }
}

fn set_process_state(process: &mut JobProcess, pid: Pid, state: ProcessState) {
    if let Some(ppid) = process.get_pid() {
        if ppid == pid {
            debug!(
                "set_process_state: {:?} pid:{:?} state:{:?}",
                &process, pid, state
            );
            process.set_state(state);
        }
    }
    if let Some(mut p) = process.next() {
        set_process_state(&mut p, pid, state);
    }
}

fn fork_process(ctx: &Context, job_pgid: Option<Pid>, process: &mut Process) -> Result<Pid> {
    // capture
    if ctx.outfile == STDOUT_FILENO && !ctx.foreground {
        let (pout, pin) = pipe().context("failed pipe")?;
        process.stdout = pin;
        process.cap_stdout = Some(pout);
    }
    if ctx.errfile == STDERR_FILENO && !ctx.foreground {
        let (pout, pin) = pipe().context("failed pipe")?;
        process.stderr = pin;
        process.cap_stderr = Some(pout);
    }

    let pid = unsafe { fork().context("failed fork")? };

    match pid {
        ForkResult::Parent { child } => {
            // if process.stdout != STDOUT_FILENO {
            //     close(process.stdout).context("failed close")?;
            // }
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
    if let Some(p) = &job.process {
        p.is_stopped()
    } else {
        true
    }
}

pub fn is_job_completed(job: &Job) -> bool {
    if let Some(p) = &job.process {
        p.is_completed()
    } else {
        true
    }
}

pub fn wait_pid(pid: Pid, no_hang: bool) -> Option<(Pid, ProcessState)> {
    let options = if no_hang {
        WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG
    } else {
        WaitPidFlag::WUNTRACED
    };

    let result = waitpid(pid, Some(options));
    let res = match result {
        Ok(WaitStatus::Exited(pid, status)) => (pid, ProcessState::Completed(status as u8)),
        Ok(WaitStatus::Signaled(pid, _signal, _)) => (pid, ProcessState::Completed(1)),
        Ok(WaitStatus::Stopped(pid, _signal)) => (pid, ProcessState::Stopped(pid)),
        Err(nix::errno::Errno::ECHILD) => (pid, ProcessState::Completed(1)),
        Ok(WaitStatus::StillAlive) => {
            return None;
        }
        status => {
            panic!("unexpected waitpid event: {:?}", status);
        }
    };
    Some(res)
}

fn create_pipe(ctx: &mut Context) -> Result<Option<RawFd>> {
    let (pout, pin) = pipe().context("failed pipe")?;
    ctx.outfile = pin;
    Ok(Some(pout))
}

fn handle_output_redirect(
    ctx: &mut Context,
    redirect: &Option<Redirect>,
    stdout: RawFd,
) -> Result<Option<RawFd>> {
    if let Some(ref output) = redirect {
        match output {
            Redirect::StdoutOutput(_) | Redirect::StdoutAppend(_) => {
                let (pout, pin) = pipe().context("failed pipe")?;
                ctx.outfile = pin;
                Ok(Some(pout))
            }
            Redirect::StderrOutput(_) | Redirect::StderrAppend(_) => {
                let (pout, pin) = pipe().context("failed pipe")?;
                ctx.errfile = pin;
                Ok(Some(pout))
            }
            Redirect::StdouterrOutput(_) | Redirect::StdouterrAppend(_) => {
                let (pout, pin) = pipe().context("failed pipe")?;
                ctx.outfile = pin;
                ctx.errfile = pin;
                Ok(Some(pout))
            }
            _ => Ok(None),
        }
    } else {
        if let Some(out) = ctx.captured_out {
            ctx.outfile = out;
        } else if ctx.infile != STDIN_FILENO {
            ctx.outfile = stdout;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {

    use nix::sys::termios::tcgetattr;
    use nix::unistd::getpgrp;

    use super::*;

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
        init();
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
        init();
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
        assert!(!is_job_stopped(job));

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
        assert!(is_job_stopped(job));
    }

    #[test]
    fn is_completed() {
        init();
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
        assert!(!is_job_completed(job));

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
        assert!(is_job_completed(job));
    }
}
