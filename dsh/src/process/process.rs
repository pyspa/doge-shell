use crate::environment::Environment;
use anyhow::{Context as _, Result};
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::unistd::Pid;
use parking_lot::RwLock;

use std::ffi::{CString, c_char};
use std::os::unix::io::RawFd;
use std::sync::Arc;
use tracing::debug;

use super::job_process::JobProcess;
use super::redirect::Redirect;
use super::state::ProcessState;
use super::wait::{WaitPidObservation, wait_pid_job};
use dsh_types::ExitStatus;

#[derive(Debug)]
pub struct PreparedExecution {
    pub cmd: CString,
    pub argv: Vec<CString>,
    pub envp: Vec<CString>,
}

impl PreparedExecution {
    /// Build the null-terminated pointer arrays `execve` needs.
    ///
    /// Parent-side only, before `fork`: the child must not allocate the
    /// `Vec<*const c_char>` itself. The returned bundle borrows nothing —
    /// the pointers reference the owned `CString`s it travels with, so keep
    /// the bundle alive across the `fork` and hand the child raw pointers
    /// into it.
    pub fn into_bundle(mut self) -> ExecveBundle {
        // `argv[0]` conventionally repeats the program; the prepared `cmd`
        // is kept as the executable path while `argv` carries the arguments.
        let mut argv_ptrs: Vec<*const c_char> = Vec::with_capacity(self.argv.len() + 1);
        for arg in &self.argv {
            argv_ptrs.push(arg.as_ptr());
        }
        argv_ptrs.push(std::ptr::null());
        let mut envp_ptrs: Vec<*const c_char> = Vec::with_capacity(self.envp.len() + 1);
        for var in &self.envp {
            envp_ptrs.push(var.as_ptr());
        }
        envp_ptrs.push(std::ptr::null());
        ExecveBundle {
            cmd: std::mem::replace(&mut self.cmd, CString::new("").expect("empty CString")),
            argv: std::mem::take(&mut self.argv),
            envp: std::mem::take(&mut self.envp),
            argv_ptrs,
            envp_ptrs,
        }
    }
}

/// Parent-preallocated `execve` image: owned strings plus pointer arrays
/// into them. The child reads `executable_ptr()`/`argv_ptr()`/`envp_ptr()`
/// without allocating.
#[derive(Debug)]
pub struct ExecveBundle {
    pub cmd: CString,
    pub argv: Vec<CString>,
    pub envp: Vec<CString>,
    argv_ptrs: Vec<*const c_char>,
    envp_ptrs: Vec<*const c_char>,
}

// Pointer arrays reference the owned `CString`s above; moving the bundle
// keeps them valid, sharing it across threads would not.
unsafe impl Send for ExecveBundle {}

impl ExecveBundle {
    pub fn executable_ptr(&self) -> *const c_char {
        self.cmd.as_ptr()
    }

    pub fn argv_ptr(&self) -> *const *const c_char {
        self.argv_ptrs.as_ptr()
    }

    pub fn envp_ptr(&self) -> *const *const c_char {
        self.envp_ptrs.as_ptr()
    }
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
    /// Redirections written on *this* command, in order. Per process,
    /// not per job: in `a 2>&1 | b` the duplication belongs to `a`.
    pub(crate) redirects: Vec<Redirect>,
    /// `NAME=value` written before this command; visible to it only.
    pub(crate) env_overrides: Vec<(String, String)>,
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
            redirects: Vec::new(),
            env_overrides: Vec::new(),
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
        let estimated_cap = env_guard.variable_state.system_env_vars.len()
            + env_guard.variable_state.exported_vars.len();
        let mut envp: Vec<CString> = Vec::with_capacity(estimated_cap + 2); // +2 for TERM, LS_COLORS fallback

        // A `NAME=value` prefix wins over both the shell's exported vars and
        // the inherited environment, and must appear only once: with a
        // duplicate key it is the *first* entry the child sees, so the
        // overridden value has to be left out rather than shadowed.
        let overridden: std::collections::HashSet<&str> = self
            .env_overrides
            .iter()
            .map(|(key, _)| key.as_str())
            .collect();

        // 1. Add system vars that are NOT overridden by exported vars
        for (key, val) in &env_guard.variable_state.system_env_vars {
            if overridden.contains(key.as_str()) {
                continue;
            }
            if !env_guard.variable_state.exported_vars.contains(key) {
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
        for key in &env_guard.variable_state.exported_vars {
            if overridden.contains(key.as_str()) {
                continue;
            }
            if let Some(value) = env_guard.variable_state.variables.get(key) {
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

        // 3. The command's own `NAME=value` prefix. `A=1 A=2 cmd` must give the
        // child `A=2`: it resolves the first duplicate, so the earlier value is
        // dropped instead of being shadowed by a later entry.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (key, value) in self.env_overrides.iter().rev() {
            if !seen.insert(key.as_str()) {
                continue;
            }
            if key == "TERM" {
                term_set = !value.is_empty();
            }
            if key == "LS_COLORS" {
                ls_colors_set = true;
            }
            if let Ok(c_str) = CString::new(format!("{key}={value}")) {
                envp.push(c_str);
            }
        }

        // Check whether TERM was already copied from system vars or exported shell vars.
        if !term_set {
            if env_guard.variable_state.exported_vars.contains("TERM") {
                // Exported but missing/empty TERM is treated as unset and falls back below.
            } else {
                if let Some(val) = env_guard.variable_state.system_env_vars.get("TERM")
                    && !val.is_empty()
                {
                    term_set = true;
                }
            }
        }

        if !ls_colors_set
            && (env_guard.variable_state.exported_vars.contains("LS_COLORS")
                || env_guard
                    .variable_state
                    .system_env_vars
                    .contains_key("LS_COLORS"))
        {
            ls_colors_set = true;
        }

        // Ensure TERM is set, falling back to xterm if missing or empty
        if !term_set {
            debug!("TERM environment variable missing, defaulting to xterm-256color");
            match CString::new("TERM=xterm-256color") {
                Ok(term_default) => envp.push(term_default),
                Err(err) => {
                    debug!("failed to construct default TERM environment variable: {err}");
                }
            }
        }

        if ls_colors_set {
            debug!("LS_COLORS is set");
        } else {
            debug!("LS_COLORS is NOT set");
        }

        Ok(PreparedExecution { cmd, argv, envp })
    }

    pub(crate) fn update_state(&mut self) -> Option<ProcessState> {
        // Only a status this caller actually observed may enter the
        // canonical tree. `NoChild` (ECHILD) means the status belongs to
        // another waiter that already consumed it — or never was ours —
        // so the existing state is kept verbatim instead of inventing an
        // exit code.
        //
        // A `Completed` head must not stop pipeline traversal: later stages
        // may still be `Running` and need polling.
        if !matches!(self.state, ProcessState::Completed(_, _))
            && let Some(pid) = self.pid
        {
            match wait_pid_job(pid, true) {
                Ok(WaitPidObservation::State(_, state)) => {
                    self.state = state;
                }
                Ok(WaitPidObservation::StillAlive) => {}
                Ok(WaitPidObservation::NoChild) => {}
                Err(nix::errno::Errno::EINTR) => {}
                Err(err) => {
                    debug!(
                        "update_state: waitpid for pid {} failed: {}; keeping {:?}",
                        pid, err, self.state
                    );
                }
            }
        }

        if let Some(next) = self.next.as_mut() {
            next.update_state();
        }

        Some(self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::builtin::BuiltinProcess;
    use dsh_types::{Context, ExitStatus};
    use nix::sys::signal::Signal;
    use nix::unistd::{Pid, getpid};
    use std::time::{Duration, Instant};

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

    /// ECHILD must not synthesize `Completed(1)`: a `Running` process whose
    /// pid is not waitable by this caller stays `Running`.
    ///
    /// The own pid deterministically yields ECHILD (it is never our child),
    /// so no timing is involved.
    #[test]
    fn update_state_does_not_synthesize_exit_one_on_echild() {
        init();
        let mut process = Process::new("test_cmd".to_string(), vec![]);
        process.pid = Some(getpid());
        assert_eq!(process.state, ProcessState::Running);
        process.update_state();
        assert_eq!(
            process.state,
            ProcessState::Running,
            "ECHILD must leave Running untouched, not invent Completed(1)"
        );
    }

    /// A `Completed` head must not stop pipeline traversal: a still-`Running`
    /// tail keeps being polled until its own `waitpid` observation completes
    /// it. The old early-return left everything behind a completed first
    /// stage unpolled forever.
    #[test]
    fn completed_head_still_updates_running_tail() {
        init();
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn tail child");
        let tail_pid = Pid::from_raw(child.id() as i32);
        // The status belongs to `update_state`, not to `Child::wait`.
        std::mem::forget(child);

        let mut head = Process::new("head".to_string(), vec![]);
        head.state = ProcessState::Completed(0, None);
        let mut tail = Process::new("tail".to_string(), vec![]);
        tail.pid = Some(tail_pid);
        head.next = Some(Box::new(JobProcess::Command(tail)));

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            head.update_state();
            let tail_state = head.next.as_deref().expect("pipeline tail").get_state();
            if matches!(tail_state, ProcessState::Completed(0, None)) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "tail behind a Completed head was never polled: {tail_state:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(head.state, ProcessState::Completed(0, None));
    }

    fn exit_zero_builtin(
        _ctx: &Context,
        _argv: Vec<String>,
        _proxy: &mut dyn dsh_builtin::ShellProxy,
    ) -> ExitStatus {
        ExitStatus::ExitedWith(0)
    }

    /// `External Completed` head must not block polling of a builtin-child
    /// tail: the re-exec helper pid is owned exactly like an external pid.
    #[test]
    fn completed_external_head_does_not_block_builtin_tail_polling() {
        init();
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn builtin-tail child");
        let tail_pid = Pid::from_raw(child.id() as i32);
        std::mem::forget(child);

        let mut head = Process::new("head".to_string(), vec![]);
        head.state = ProcessState::Completed(0, None);
        let mut tail = BuiltinProcess::new(
            "dirs".to_string(),
            exit_zero_builtin,
            vec!["dirs".to_string()],
        );
        tail.pid = Some(tail_pid);
        head.next = Some(Box::new(JobProcess::Builtin(tail)));

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            head.update_state();
            let tail_state = head.next.as_deref().expect("pipeline tail").get_state();
            if matches!(tail_state, ProcessState::Completed(0, None)) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "builtin tail behind a Completed head was never polled: {tail_state:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(head.state, ProcessState::Completed(0, None));
    }

    /// A `Stopped` state is real observed state and must survive ECHILD too.
    #[test]
    fn update_state_preserves_stopped_on_echild() {
        init();
        let mut process = Process::new("test_cmd".to_string(), vec![]);
        let stopped = ProcessState::Stopped(getpid(), Signal::SIGTSTP);
        process.pid = Some(getpid());
        process.state = stopped;
        process.update_state();
        assert_eq!(
            process.state, stopped,
            "ECHILD must leave Stopped untouched, not invent Completed(1)"
        );
    }
    #[test]
    fn test_prepare_execution() {
        init();
        let env_arc = Environment::new();
        {
            let mut env = env_arc.write();
            env.variable_state
                .variables
                .insert("TEST_VAR".to_string(), "test_value".to_string());
            env.variable_state
                .exported_vars
                .insert("TEST_VAR".to_string());
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
            env.variable_state.system_env_vars.clear();

            // 1. Set a system var
            env.variable_state
                .system_env_vars
                .insert("SYSTEM_VAR".into(), "sys_val".into());
            // 2. Set an exported var
            env.variable_state
                .exported_vars
                .insert("EXPORTED_VAR".into());
            env.variable_state
                .variables
                .insert("EXPORTED_VAR".into(), "exp_val".into());
            // 3. Set a var that is both (override)
            env.variable_state
                .system_env_vars
                .insert("OVERRIDDEN".into(), "old_val".into());
            env.variable_state.exported_vars.insert("OVERRIDDEN".into());
            env.variable_state
                .variables
                .insert("OVERRIDDEN".into(), "new_val".into());
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
            env.variable_state.system_env_vars.clear();
            env.variable_state
                .system_env_vars
                .insert("TERM".into(), "dumb".into());
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
            env.variable_state.system_env_vars.clear();
            env.variable_state.exported_vars.insert("TERM".into());
            env.variable_state
                .variables
                .insert("TERM".into(), "".into());
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
            env.variable_state.system_env_vars.clear();
            env.variable_state
                .system_env_vars
                .insert("TERM".into(), "".into());
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
