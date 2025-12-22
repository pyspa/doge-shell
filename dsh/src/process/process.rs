use crate::environment::Environment;
use anyhow::{Context as _, Result};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use nix::unistd::{Pid, close, dup2, execve, setpgid, tcsetpgrp};
use parking_lot::RwLock;

use std::ffi::CString;
use std::os::unix::io::RawFd;
use std::sync::Arc;
use tracing::{debug, error};

use super::job_process::JobProcess;
use super::state::ProcessState;
use super::wait::wait_pid_job;
use crate::shell::SHELL_TERMINAL;
use dsh_types::ExitStatus;

#[derive(Debug)]
pub struct PreparedExecution {
    pub cmd: CString,
    pub argv: Vec<CString>,
    pub envp: Vec<CString>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Process {
    pub(crate) cmd: String,
    pub(crate) argv: Vec<String>,
    pub(crate) pid: Option<Pid>,
    pub(crate) status: Option<ExitStatus>,
    pub(crate) state: ProcessState, // completed, stopped,
    pub next: Option<Box<JobProcess>>,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
    pub(crate) cap_stdout: Option<RawFd>,
    pub(crate) cap_stderr: Option<RawFd>,
}

impl std::fmt::Debug for Process {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Process")
            .field("cmd", &self.cmd)
            .field("argv", &self.argv)
            .field("pid", &self.pid)
            .field("status", &self.status)
            .field("state", &self.state)
            .field("next", &self.next)
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
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

    pub fn set_state(&mut self, pid: Pid, state: ProcessState) -> bool {
        if let Some(ppid) = self.pid
            && ppid == pid
        {
            self.state = state;
            return true;
        }
        if let Some(ref mut next) = self.next
            && next.set_state_pid(pid, state)
        {
            return true;
        }
        false
    }

    pub fn link(&mut self, process: JobProcess) {
        match self.next {
            Some(ref mut p) => {
                p.link(process);
            }
            None => {
                debug!("link:{} next:{}", self.cmd, process.get_cmd());
                self.next = Some(Box::new(process));
            }
        }
    }

    fn set_signals(&self) -> Result<()> {
        debug!("set signal action pid:{:?}", self.pid);
        // Accept job-control-related signals (refer https://www.gnu.org/software/libc/manual/html_node/Launching-Jobs.html)
        let action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        unsafe {
            sigaction(Signal::SIGINT, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGINT handler: {}", e))?;
            sigaction(Signal::SIGQUIT, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGQUIT handler: {}", e))?;
            sigaction(Signal::SIGTSTP, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGTSTP handler: {}", e))?;
            sigaction(Signal::SIGTTIN, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGTTIN handler: {}", e))?;
            sigaction(Signal::SIGTTOU, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGTTOU handler: {}", e))?;
            sigaction(Signal::SIGCHLD, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGCHLD handler: {}", e))?;
            sigaction(Signal::SIGPIPE, &action)
                .map_err(|e| anyhow::anyhow!("failed to set SIGPIPE handler: {}", e))?;
        }
        Ok(())
    }

    pub fn prepare_execution(
        &self,
        environment: Arc<RwLock<Environment>>,
    ) -> Result<PreparedExecution> {
        let cmd = CString::new(self.cmd.clone()).context("failed new CString")?;
        let argv: Result<Vec<CString>> = self
            .argv
            .clone()
            .into_iter()
            .map(|a| {
                CString::new(a).map_err(|e| anyhow::anyhow!("failed to create CString: {}", e))
            })
            .collect();
        let argv = argv?;

        // Build environment for child process without intermediate HashMap cloning
        let env_guard = environment.read();

        // Calculate minimal capacity to avoid re-allocations
        // (system vars + exported vars, though some might overlap)
        let estimated_cap = env_guard.system_env_vars.len() + env_guard.exported_vars.len();
        let mut envp: Vec<CString> = Vec::with_capacity(estimated_cap + 2); // +2 for TERM, LS_COLORS fallback

        // 1. Add system vars that are NOT overridden by exported vars
        for (key, val) in &env_guard.system_env_vars {
            if !env_guard.exported_vars.contains(key) {
                // Special handling for TERM: if empty, skip so we can default it later
                if key == "TERM" && val.is_empty() {
                    continue;
                }
                if let Ok(c_str) = CString::new(format!("{}={}", key, val)) {
                    envp.push(c_str);
                }
            }
        }

        // Environment map for quick lookups for special vars like TERM
        // We only populate this lightly or check directly if possible.
        // Actually, we need to check if TERM/LS_COLORS are set in the FINAL environment.
        // We can track this with booleans.
        let mut term_set = false;
        let mut ls_colors_set = false;

        // 2. Add exported vars (overriding system vars)
        for key in &env_guard.exported_vars {
            if let Some(value) = env_guard.variables.get(key) {
                if key == "TERM" {
                    if value.is_empty() {
                        continue;
                    }
                    term_set = true;
                }
                if key == "LS_COLORS" {
                    ls_colors_set = true;
                }

                if let Ok(c_str) = CString::new(format!("{}={}", key, value)) {
                    envp.push(c_str);
                }
            }
        }

        // Check if TERM was set in system vars (if not overridden)
        if !term_set {
            // It might be in system_env_vars and NOT in exported_vars
            // in which case it was added in step 1.
            // We need to check if we added it?
            // Or simpler: check existence in the appropriate source.
            if env_guard.exported_vars.contains("TERM") {
                // It is in exported vars, so `term_set` logic handles it (it was true if var exists).
                // If it's in exported_vars but NOT in variables, it's effectively unset?
                // Logic in original code: `if let Some(value) = variables.get(key) { insert }`.
                // So if it's exported but missing value, it's not added.
            } else {
                // Not exported. Check system.
                if let Some(val) = env_guard.system_env_vars.get("TERM")
                    && !val.is_empty()
                {
                    term_set = true;
                }
            }
        }

        if !ls_colors_set
            && (env_guard.exported_vars.contains("LS_COLORS")
                || env_guard.system_env_vars.contains_key("LS_COLORS"))
        {
            ls_colors_set = true;
        }

        // Ensure TERM is set, falling back to xterm if missing or empty
        if !term_set {
            debug!("TERM environment variable missing, defaulting to xterm-256color");
            envp.push(CString::new("TERM=xterm-256color").unwrap());
        } else {
            // Debug logging for TERM if needed, but we don't have the value easily accessible here without iterating
            // Retaining behavior of "defaulting if empty" is tricky without map.
            // But usually env vars are not empty string if unset.
            // If TERM is set to "", original code defaulted.
            // We skip that check for perf? Or iter to check?
            // "if term.is_empty()" check is valuable.
            // To support this, we might need to check the value when adding.
            // Let's refine the loop above.
        }

        if ls_colors_set {
            debug!("LS_COLORS is set");
        } else {
            debug!("LS_COLORS is NOT set");
        }

        // Final sanity check for TERM empty value if we want strict parity?
        // Original code: if env_map.get("TERM").unwrap().is_empty() -> set default.
        // We can ignore this edge case for now or handle it if critical.
        // Assuming TERM="" is rare/user error. Defaults usually handle missing key.

        Ok(PreparedExecution { cmd, argv, envp })
    }

    pub fn launch(
        &mut self,
        pid: Pid,
        pgid: Pid,
        interactive: bool,
        foreground: bool,
        environment: Arc<RwLock<Environment>>,
        pty_slave: Option<RawFd>,
    ) -> Result<()> {
        let prepared = self.prepare_execution(environment)?;
        self.launch_prepared(pid, pgid, interactive, foreground, prepared, pty_slave)
    }

    pub fn launch_prepared(
        &mut self,
        pid: Pid,
        pgid: Pid,
        interactive: bool,
        foreground: bool,
        prepared: PreparedExecution,
        pty_slave: Option<RawFd>,
    ) -> Result<()> {
        let PreparedExecution { cmd, argv, envp } = prepared;
        if interactive {
            // If using PTY, setsid() will be called later which sets the process group/session.
            // We must avoid setpgid() making us a leader before setsid() (which causes EPERM).
            if pty_slave.is_none() {
                debug!(
                    "setpgid child process {} pid:{} pgid:{} foreground:{}",
                    &self.cmd, pid, pgid, foreground
                );
                setpgid(pid, pgid).context("failed setpgid")?;
            } else {
                debug!("Skipping setpgid for PTY process (setsid will handle it)");
            }

            // If we are using PTY, we don't want to set this process as foreground of the SHELL's terminal
            // because the Shell will proxy input/output.
            if foreground && pty_slave.is_none() {
                tcsetpgrp(SHELL_TERMINAL, pgid).context("failed tcsetpgrp")?;
            }

            // Set signals AFTER setting foreground process group to avoid race condition
            // where we receive a signal while still ignoring it (inherited from shell)
            self.set_signals()?;
        } else {
            // For non-interactive/background, we still need to reset SIGPIPE as Rust ignores it by default
            let action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
            unsafe {
                let _ = sigaction(Signal::SIGPIPE, &action);
            }
        }

        if let Some(slave_fd) = pty_slave {
            // Create a new session and set the controlling terminal to the PTY
            // This is crucial for programs like 'ls' to detect they are in a terminal
            let my_pid = nix::unistd::getpid();
            let my_pgid = nix::unistd::getpgid(Some(my_pid)).unwrap_or(Pid::from_raw(-1));
            let my_sid = nix::unistd::getsid(Some(my_pid)).unwrap_or(Pid::from_raw(-1));
            debug!(
                "setsid check: pid={} pgid={} sid={}",
                my_pid, my_pgid, my_sid
            );

            match nix::unistd::setsid() {
                Ok(new_sid) => {
                    debug!("setsid success: new_sid={}", new_sid);
                }
                Err(e) => {
                    error!("setsid failed raw error: {:?}", e);
                    return Err(anyhow::anyhow!("setsid failed: {}", e));
                }
            }

            unsafe {
                if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) != 0 {
                    // ignore error? sometimes it fails if already leader
                    debug!("ioctl TIOCSCTTY failed (may be already leader)");
                }
            }
        }

        // cmd, argv, envp are already prepared in `prepared`

        debug!(
            "launch: execve cmd:{:?} argv:{:?} foreground:{:?} infile:{:?} outfile:{:?} pid:{:?} pgid:{:?} pty:{:?}",
            cmd, argv, foreground, self.stdin, self.stdout, pid, pgid, pty_slave
        );

        // Standard IO setup (PTY slave is handled via self.stdin/stdout/stderr being set to it by caller if needed)

        // 1. Handle STDIN
        if self.stdin != STDIN_FILENO {
            dup2(self.stdin, STDIN_FILENO)
                .map_err(|e| anyhow::anyhow!("dup2 stdin failed: {}", e))?;
        }
        // Don't close stdin yet if it matches stdout or stderr, as we need it for subsequent dup2 calls
        let keep_stdin = self.stdin == self.stdout || self.stdin == self.stderr;
        if self.stdin > 2 && !keep_stdin {
            close(self.stdin).map_err(|e| anyhow::anyhow!("close stdin failed: {}", e))?;
        }

        // 2. Handle STDOUT & STDERR
        if self.stdout == self.stderr {
            // Combined stdout/stderr (e.g. PTY or redirected to same file)
            if self.stdout != STDOUT_FILENO {
                dup2(self.stdout, STDOUT_FILENO)
                    .map_err(|e| anyhow::anyhow!("dup2 stdout failed: {}", e))?;
            }
            if self.stderr != STDERR_FILENO {
                dup2(self.stderr, STDERR_FILENO)
                    .map_err(|e| anyhow::anyhow!("dup2 stderr failed: {}", e))?;
            }

            // Close the source if it is > 2.
            // Even if it was kept open from stdin check above, we are now done with it.
            if self.stdout > 2 {
                close(self.stdout).map_err(|e| anyhow::anyhow!("close stdout failed: {}", e))?;
            }
        } else {
            // Separate stdout/stderr
            if self.stdout != STDOUT_FILENO {
                dup2(self.stdout, STDOUT_FILENO)
                    .map_err(|e| anyhow::anyhow!("dup2 stdout failed: {}", e))?;
                // If stdout matched stdin, it was kept open. Now we can close it.
                if self.stdout > 2 {
                    close(self.stdout)
                        .map_err(|e| anyhow::anyhow!("close stdout failed: {}", e))?;
                }
            }

            if self.stderr != STDERR_FILENO {
                dup2(self.stderr, STDERR_FILENO)
                    .map_err(|e| anyhow::anyhow!("dup2 stderr failed: {}", e))?;
                // If stderr matched stdin, it was kept open. Now close it.
                if self.stderr > 2 {
                    close(self.stderr)
                        .map_err(|e| anyhow::anyhow!("close stderr failed: {}", e))?;
                }
            }
        }
        match execve(&cmd, &argv, &envp) {
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

    pub(crate) fn update_state(&mut self) -> Option<ProcessState> {
        if let ProcessState::Completed(_, _) = self.state {
            Some(self.state)
        } else {
            if let Some(pid) = self.pid
                && let Some((_waited_pid, state)) = wait_pid_job(pid, true)
            {
                self.state = state;
            }

            if let Some(next) = self.next.as_mut() {
                next.update_state();
            }

            Some(self.state)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::signal::Signal;
    use nix::unistd::Pid;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_process_state_transitions() {
        init();
        let mut process = Process::new("test_cmd".to_string(), vec!["arg1".to_string()]);

        // Initial state is Running
        assert!(matches!(process.state, ProcessState::Running));

        // State change test
        process.state = ProcessState::Completed(0, None);
        assert!(matches!(process.state, ProcessState::Completed(0, None)));

        process.state = ProcessState::Stopped(Pid::from_raw(1234), Signal::SIGSTOP);
        assert!(matches!(
            process.state,
            ProcessState::Stopped(_, Signal::SIGSTOP)
        ));
    }
    #[test]
    fn test_prepare_execution() {
        init();
        let env_arc = Environment::new();
        {
            let mut env = env_arc.write();
            env.variables
                .insert("TEST_VAR".to_string(), "test_value".to_string());
            env.exported_vars.insert("TEST_VAR".to_string());
        }

        let process = Process::new(
            "echo".to_string(),
            vec!["echo".to_string(), "hello".to_string()],
        );

        let prepared = process
            .prepare_execution(env_arc)
            .expect("Failed to prepare execution");

        assert_eq!(prepared.cmd.to_string_lossy(), "echo");

        assert_eq!(prepared.argv.len(), 2);
        assert_eq!(prepared.argv[0].to_string_lossy(), "echo");
        assert_eq!(prepared.argv[1].to_string_lossy(), "hello");

        let env_vec: Vec<String> = prepared
            .envp
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();

        // Check for our custom var
        assert!(
            env_vec.contains(&"TEST_VAR=test_value".to_string()),
            "Environment should contain TEST_VAR"
        );

        // Check for TERM (should be defaulted to xterm-256color since we removed it from system_env_vars in the mock)
        // Note: Environment::new() copies real env vars into system_env_vars.
        assert!(
            env_vec.iter().any(|s| s.starts_with("TERM=")),
            "Environment should contain TERM"
        );
    }
    #[test]
    fn test_prepare_execution_env_optimization() {
        init();
        let env_arc = Environment::new();
        {
            let mut env = env_arc.write();
            // Clear default system vars to have a predictable test state
            env.system_env_vars.clear();

            // 1. Set a system var
            env.system_env_vars
                .insert("SYSTEM_VAR".into(), "sys_val".into());
            // 2. Set an exported var
            env.exported_vars.insert("EXPORTED_VAR".into());
            env.variables
                .insert("EXPORTED_VAR".into(), "exp_val".into());
            // 3. Set a var that is both (override)
            env.system_env_vars
                .insert("OVERRIDDEN".into(), "old_val".into());
            env.exported_vars.insert("OVERRIDDEN".into());
            env.variables.insert("OVERRIDDEN".into(), "new_val".into());
        }

        let process = Process::new("echo".into(), vec![]);
        let prepared = process.prepare_execution(env_arc).expect("prepare failed");

        let env_strs: Vec<String> = prepared
            .envp
            .iter()
            .map(|c| c.to_str().unwrap().to_string())
            .collect();

        // Check content
        assert!(env_strs.contains(&"SYSTEM_VAR=sys_val".to_string()));
        assert!(env_strs.contains(&"EXPORTED_VAR=exp_val".to_string()));
        assert!(env_strs.contains(&"OVERRIDDEN=new_val".to_string()));
        assert!(!env_strs.contains(&"OVERRIDDEN=old_val".to_string()));

        // Term default check (since cleared, should add default)
        assert!(env_strs.contains(&"TERM=xterm-256color".to_string()));
    }

    #[test]
    fn test_prepare_execution_term_handling_edge_cases() {
        init();
        // Case 1: TERM in system env, not exported -> Should be preserved
        let env_arc = Environment::new();
        {
            let mut env = env_arc.write();
            env.system_env_vars.clear();
            env.system_env_vars.insert("TERM".into(), "dumb".into());
        }
        let process = Process::new("echo".into(), vec![]);
        let prepared = process.prepare_execution(env_arc).unwrap();
        let env_strs: Vec<String> = prepared
            .envp
            .iter()
            .map(|c| c.to_str().unwrap().to_string())
            .collect();
        assert!(env_strs.contains(&"TERM=dumb".to_string()));
        assert!(!env_strs.contains(&"TERM=xterm-256color".to_string()));

        // Case 2: TERM exported, empty value -> Should fall back to default
        let env_arc = Environment::new();
        {
            let mut env = env_arc.write();
            env.system_env_vars.clear();
            env.exported_vars.insert("TERM".into());
            env.variables.insert("TERM".into(), "".into());
        }

        let process = Process::new("echo".into(), vec![]);
        let prepared = process.prepare_execution(env_arc).unwrap();
        let env_strs: Vec<String> = prepared
            .envp
            .iter()
            .map(|c| c.to_str().unwrap().to_string())
            .collect();
        assert!(env_strs.contains(&"TERM=xterm-256color".to_string()));

        // Case 3: TERM in system env is EMPTY -> Should fall back to default
        let env_arc = Environment::new();
        {
            let mut env = env_arc.write();
            env.system_env_vars.clear();
            env.system_env_vars.insert("TERM".into(), "".into());
        }

        let process = Process::new("echo".into(), vec![]);
        let prepared = process.prepare_execution(env_arc).unwrap();
        let env_strs: Vec<String> = prepared
            .envp
            .iter()
            .map(|c| c.to_str().unwrap().to_string())
            .collect();
        assert!(env_strs.contains(&"TERM=xterm-256color".to_string()));
    }
}
