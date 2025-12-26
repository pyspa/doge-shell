use crate::{process::ProcessState, shell::Shell};
use anyhow::{Context as _, Result};
use dsh_builtin::ShellProxy;
use dsh_frecency::SortMethod;
use dsh_types::{Context, mcp::McpServerConfig};
use std::fs::File;
use std::io::prelude::*;
use std::os::unix::io::FromRawFd;
use tabled::{Table, Tabled};
use tracing::{debug, error, warn};

#[derive(Tabled)]
struct Job {
    job: usize,
    pid: i32,
    state: String,
    command: String,
}

#[derive(Tabled)]
struct Var {
    key: String,
    value: String,
}

/// Parse job specification (e.g., "%1", "1", "%+", "%-")
/// Returns the job index in wait_jobs vector, or None if not found
fn parse_job_spec(spec: &str, wait_jobs: &[crate::process::Job]) -> Option<usize> {
    if spec.is_empty() {
        // Default to most recent job
        return if wait_jobs.is_empty() {
            None
        } else {
            Some(wait_jobs.len() - 1)
        };
    }

    let spec = spec.trim();

    // Handle %+ (current job) and %- (previous job)
    if spec == "%+" || spec == "+" {
        return if wait_jobs.is_empty() {
            None
        } else {
            Some(wait_jobs.len() - 1)
        };
    }
    if spec == "%-" || spec == "-" {
        return if wait_jobs.len() < 2 {
            None
        } else {
            Some(wait_jobs.len() - 2)
        };
    }

    // Handle %n or n format (job number)
    let job_num_str = if let Some(stripped) = spec.strip_prefix('%') {
        stripped
    } else {
        spec
    };

    if let Ok(job_num) = job_num_str.parse::<usize>() {
        // Find job by job_id
        for (index, job) in wait_jobs.iter().enumerate() {
            if job.job_id == job_num {
                return Some(index);
            }
        }
    }

    None
}

/// Format reload error messages based on error type for better user experience
fn format_reload_error(err: &anyhow::Error) -> String {
    let error_string = err.to_string();

    // Handle file not found errors
    if error_string.contains("No such file or directory")
        || error_string.contains("Failed to read config file")
    {
        if let Some(path_start) = error_string.find("~/.config/dsh/config.lisp") {
            let path_end = path_start + "~/.config/dsh/config.lisp".len();
            let config_path = &error_string[path_start..path_end];
            return format!("reload: file not found: {config_path}");
        } else if let Some(path_start) = error_string.rfind('/') {
            // Extract just the filename if full path is shown
            if let Some(path_end) = error_string[path_start..].find(' ') {
                let filename = &error_string[path_start + 1..path_start + path_end];
                return format!("reload: file not found: ~/.config/dsh/{filename}");
            }
        }
        return "reload: file not found: ~/.config/dsh/config.lisp".to_string();
    }

    // Handle permission denied errors
    if error_string.contains("Permission denied") {
        return "reload: permission denied: cannot read ~/.config/dsh/config.lisp".to_string();
    }

    // Handle XDG directory errors
    if error_string.contains("failed get xdg directory") {
        return "reload: configuration directory error: unable to access ~/.config/dsh/"
            .to_string();
    }

    // Handle Lisp parsing errors
    if error_string.contains("Parse error:") {
        // Extract the parse error details
        if let Some(parse_start) = error_string.find("Parse error:") {
            let parse_error = &error_string[parse_start..];
            return format!(
                "reload: syntax error: {}",
                parse_error.trim_start_matches("Parse error: ")
            );
        }
        return format!("reload: syntax error: {error_string}");
    }

    // Handle Lisp runtime errors
    if error_string.contains("Runtime error:") {
        // Extract the runtime error details
        if let Some(runtime_start) = error_string.find("Runtime error:") {
            let runtime_error = &error_string[runtime_start..];
            return format!(
                "reload: runtime error: {}",
                runtime_error.trim_start_matches("Runtime error: ")
            );
        }
        return format!("reload: runtime error: {error_string}");
    }

    // Handle other I/O errors
    if error_string.contains("I/O error") || error_string.contains("io::Error") {
        return format!("reload: I/O error: {error_string}");
    }

    // Generic error fallback with reload prefix
    format!("reload: {error_string}")
}

/// Parse arguments for z command
/// Returns (interactive, query)
fn parse_z_args(argv: &[String]) -> (bool, String) {
    let mut interactive = false;
    let mut query = String::new();

    // Start from index 1, skip command name
    for arg in argv.iter().skip(1) {
        if arg == "-i" || arg == "--interactive" {
            interactive = true;
        } else if query.is_empty() {
            query = arg.clone();
        }
    }
    (interactive, query)
}

impl ShellProxy for Shell {
    fn exit_shell(&mut self) {
        self.exit();
    }

    fn save_path_history(&mut self, path: &str) {
        if let Some(ref mut history) = self.path_history {
            let mut history = history.lock(); // For non-UI operations, we can use regular lock
            history.add(path);
        }
    }

    fn changepwd(&mut self, path: &str) -> Result<()> {
        // Save current directory as OLDPWD before changing
        if let Ok(current) = std::env::current_dir() {
            let old_pwd = current.to_string_lossy().into_owned();
            self.environment
                .write()
                .variables
                .insert("OLDPWD".to_string(), old_pwd);
        }

        std::env::set_current_dir(path)?;
        self.save_path_history(path);
        self.exec_chpwd_hooks(path)?;
        Ok(())
    }

    fn insert_path(&mut self, idx: usize, path: &str) {
        self.environment.write().paths.insert(idx, path.to_string());
    }

    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()> {
        match cmd {
            "exit" => {
                self.exit();
            }
            "history" => {
                if let Some(ref mut history) = self.cmd_history {
                    let mut history = history.lock(); // For non-UI operations, we can use regular lock
                    for item in history.iter() {
                        ctx.write_stdout(&item.entry)?;
                    }
                    history.reset_index();
                }
            }
            "z" => {
                let (interactive, query) = parse_z_args(&argv);

                if interactive {
                    if let Some(ref mut history) = self.path_history {
                        let history = history.clone();
                        let history = history.lock();
                        // Get all history items or filter by query if provided
                        let results = if query.is_empty() {
                            history.sorted(&SortMethod::Recent)
                        } else {
                            history.sort_by_match(&query)
                        };

                        if !results.is_empty() {
                            // Convert to Candidates for skim
                            let candidates: Vec<crate::completion::Candidate> = results
                                .iter()
                                .map(|item| {
                                    crate::completion::Candidate::Item(
                                        item.item.clone(),
                                        format!("({:.1})", item.match_score),
                                    )
                                })
                                .collect();

                            if let Some(selected) =
                                crate::completion::select_item_with_skim(candidates, None)
                            {
                                self.changepwd(&selected)?;
                            }
                        } else {
                            ctx.write_stderr("z: no matching history found")?;
                        }
                    } else {
                        ctx.write_stderr("z: history not available")?;
                    }
                } else {
                    // Original behavior
                    let path = if query.is_empty() { None } else { Some(query) };

                    let path = if let Some(path_str) = path {
                        if let Some(ref mut history) = self.path_history {
                            let history = history.clone();
                            let history = history.lock();
                            let results = history.sort_by_match(&path_str);
                            if !results.is_empty() {
                                Some(results[0].item.to_string())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(ref path) = path {
                        self.changepwd(path)?;
                    };
                }
            }
            "jobs" => {
                if self.wait_jobs.is_empty() {
                    ctx.write_stdout("jobs: there are no jobs")?;
                } else {
                    let jobs: Vec<Job> = self
                        .wait_jobs
                        .iter()
                        .map(|job| Job {
                            job: job.job_id,
                            pid: job.pid.map(|p| p.as_raw()).unwrap_or(-1),
                            state: format!("{}", job.state),
                            command: job.cmd.clone(),
                        })
                        .collect();
                    let table = Table::new(jobs).to_string();
                    ctx.write_stdout(table.as_str())?;
                }
            }
            "lisp" => match self.lisp_engine.borrow().run(argv[1].as_str()) {
                Ok(val) => {
                    debug!("{}", val);
                }
                Err(err) => {
                    ctx.write_stderr(&format!("{err}"))?;
                }
            },
            "lisp-run" => {
                let mut argv = argv;
                let cmd = argv.remove(0);
                match self.lisp_engine.borrow().run_func(cmd.as_str(), argv) {
                    Ok(val) => {
                        debug!("{}", val);
                    }
                    Err(err) => {
                        ctx.write_stderr(&format!("{err}"))?;
                    }
                }
            }
            "var" => {
                let vars: Vec<Var> = self
                    .environment
                    .read()
                    .variables
                    .iter()
                    .map(|x| Var {
                        key: x.0.to_owned(),
                        value: x.1.to_owned(),
                    })
                    .collect();
                let table = Table::new(vars).to_string();
                ctx.write_stdout(table.as_str())?;
            }
            "read" => {
                let mut stdin = Vec::new();
                unsafe {
                    File::from_raw_fd(ctx.infile)
                        .read_to_end(&mut stdin)
                        .context("read: failed to read input")?;
                };
                let key = format!("${}", argv[1]);
                let output = match std::str::from_utf8(&stdin) {
                    Ok(s) => s.trim_end_matches('\n').to_owned(),
                    Err(err) => {
                        ctx.write_stderr(&format!("read: invalid UTF-8 input: {err}"))
                            .ok();
                        return Err(anyhow::anyhow!("invalid UTF-8 input: {}", err));
                    }
                };

                self.environment.write().variables.insert(key, output);
            }
            "fg" => {
                debug!(
                    "FG_CMD_START: Starting fg command - wait_jobs.len(): {}, args: {:?}",
                    self.wait_jobs.len(),
                    argv
                );

                if self.wait_jobs.is_empty() {
                    debug!("FG_CMD_NO_JOBS: No jobs available for fg command");
                    ctx.write_stdout("fg: there are no suitable jobs")?;
                } else {
                    let job_spec = argv.get(1).map(|s| s.as_str()).unwrap_or("");
                    debug!("FG_CMD_SPEC: Job specification: '{}'", job_spec);

                    // Log current job list for debugging
                    debug!("FG_CMD_AVAILABLE_JOBS: Current job list:");
                    for (i, job) in self.wait_jobs.iter().enumerate() {
                        debug!(
                            "FG_CMD_JOB[{}]: id={}, pid={:?}, state={:?}, foreground={}, cmd='{}'",
                            i, job.job_id, job.pid, job.state, job.foreground, job.cmd
                        );
                    }

                    if let Some(job_index) = parse_job_spec(job_spec, &self.wait_jobs) {
                        let mut job = self.wait_jobs.remove(job_index);
                        debug!(
                            "FG_CMD_SELECTED: Selected job {} at index {} for foreground",
                            job.job_id, job_index
                        );
                        debug!(
                            "FG_CMD_JOB_DETAILS: Job details before fg - state: {:?}, pgid: {:?}, pid: {:?}",
                            job.state, job.pgid, job.pid
                        );

                        ctx.write_stdout(&format!(
                            "dsh: job {} '{}' to foreground",
                            job.job_id, job.cmd
                        ))
                        .ok();

                        let cont = if let ProcessState::Stopped(_, _) = job.state {
                            debug!(
                                "FG_CMD_STOPPED: Job {} is stopped, will send SIGCONT",
                                job.job_id
                            );
                            true
                        } else {
                            debug!(
                                "FG_CMD_NOT_STOPPED: Job {} is not stopped, no SIGCONT needed (state: {:?})",
                                job.job_id, job.state
                            );
                            false
                        };

                        let old_state = job.state;
                        job.state = ProcessState::Running;
                        debug!(
                            "FG_CMD_STATE_CHANGE: Set job {} state from {:?} to Running",
                            job.job_id, old_state
                        );

                        debug!(
                            "FG_CMD_FOREGROUND_CALL: About to call put_in_foreground_sync for job {} with no_hang=true, cont={}",
                            job.job_id, cont
                        );

                        match job.put_in_foreground_sync(true, cont) {
                            Ok(_) => {
                                debug!(
                                    "FG_CMD_SUCCESS: put_in_foreground_sync completed successfully for job {}",
                                    job.job_id
                                );
                            }
                            Err(err) => {
                                error!(
                                    "FG_CMD_ERROR: put_in_foreground_sync failed for job {} with error: {:?}",
                                    job.job_id, err
                                );
                                ctx.write_stderr(&format!("{err}")).ok();
                                return Err(err);
                            }
                        }
                    } else {
                        let error_msg = if job_spec.is_empty() {
                            "fg: no current job".to_string()
                        } else {
                            format!("fg: job not found: {job_spec}")
                        };
                        debug!("FG_CMD_NOT_FOUND: {}", error_msg);
                        ctx.write_stderr(&error_msg)?;
                        return Err(anyhow::anyhow!(error_msg));
                    }
                }
            }
            "bg" => {
                debug!(
                    "BG_CMD_START: Starting bg command - wait_jobs.len(): {}, args: {:?}",
                    self.wait_jobs.len(),
                    argv
                );

                if self.wait_jobs.is_empty() {
                    debug!("BG_CMD_NO_JOBS: No jobs available for bg command");
                    ctx.write_stdout("bg: there are no suitable jobs")?;
                } else {
                    let job_spec = argv.get(1).map(|s| s.as_str()).unwrap_or("");
                    debug!("BG_CMD_SPEC: Job specification: '{}'", job_spec);

                    // Log current job list for debugging
                    debug!("BG_CMD_AVAILABLE_JOBS: Current job list:");
                    for (i, job) in self.wait_jobs.iter().enumerate() {
                        debug!(
                            "BG_CMD_JOB[{}]: id={}, pid={:?}, state={:?}, foreground={}, cmd='{}'",
                            i, job.job_id, job.pid, job.state, job.foreground, job.cmd
                        );
                    }

                    // Find job by specification or default to most recent stopped job
                    let job_index = if job_spec.is_empty() {
                        debug!("BG_CMD_FIND_STOPPED: Looking for most recent stopped job");
                        // Find the most recent stopped job
                        let mut found_index = None;
                        for (i, job) in self.wait_jobs.iter().enumerate().rev() {
                            debug!(
                                "BG_CMD_CHECK_STOPPED: Checking job {} (index: {}, state: {:?})",
                                job.job_id, i, job.state
                            );
                            if matches!(job.state, ProcessState::Stopped(_, _)) {
                                debug!(
                                    "BG_CMD_FOUND_STOPPED: Found stopped job {} at index {}",
                                    job.job_id, i
                                );
                                found_index = Some(i);
                                break;
                            }
                        }
                        if found_index.is_none() {
                            debug!("BG_CMD_NO_STOPPED: No stopped jobs found");
                        }
                        found_index
                    } else {
                        debug!(
                            "BG_CMD_PARSE_SPEC: Parsing job specification: '{}'",
                            job_spec
                        );
                        // Parse job specification
                        parse_job_spec(job_spec, &self.wait_jobs)
                    };

                    if let Some(index) = job_index {
                        let job = &self.wait_jobs[index];
                        debug!(
                            "BG_CMD_SELECTED: Selected job {} at index {} for background",
                            job.job_id, index
                        );

                        // Check if job is actually stopped
                        if !matches!(job.state, ProcessState::Stopped(_, _)) {
                            let error_msg = format!("bg: job {} is already running", job.job_id);
                            debug!("BG_CMD_ALREADY_RUNNING: {}", error_msg);
                            ctx.write_stderr(&error_msg)?;
                            return Err(anyhow::anyhow!(error_msg));
                        }

                        let mut job = self.wait_jobs.remove(index);
                        debug!(
                            "BG_CMD_JOB_DETAILS: Job details before bg - state: {:?}, pgid: {:?}, pid: {:?}",
                            job.state, job.pgid, job.pid
                        );

                        ctx.write_stdout(&format!(
                            "dsh: job {} '{}' to background",
                            job.job_id, job.cmd
                        ))
                        .ok();

                        // Set job state to running and send SIGCONT
                        let old_state = job.state;
                        job.state = ProcessState::Running;
                        debug!(
                            "BG_CMD_STATE_CHANGE: Set job {} state from {:?} to Running",
                            job.job_id, old_state
                        );

                        // Send SIGCONT to resume the job
                        if let Some(pgid) = job.pgid {
                            debug!(
                                "BG_CMD_SIGCONT: Sending SIGCONT to process group {} for job {}",
                                pgid, job.job_id
                            );
                            use nix::sys::signal::{Signal, killpg};
                            match killpg(pgid, Signal::SIGCONT) {
                                Ok(_) => {
                                    debug!(
                                        "BG_CMD_SIGCONT_SUCCESS: SIGCONT sent successfully to job {}",
                                        job.job_id
                                    );
                                }
                                Err(err) => {
                                    error!(
                                        "BG_CMD_SIGCONT_ERROR: Failed to send SIGCONT to job {}: {}",
                                        job.job_id, err
                                    );
                                    ctx.write_stderr(&format!("bg: failed to resume job: {err}"))
                                        .ok();
                                    return Err(err.into());
                                }
                            }
                        } else {
                            warn!(
                                "BG_CMD_NO_PGID: Job {} has no process group ID, cannot send SIGCONT",
                                job.job_id
                            );
                        }

                        // Put the job back in the background jobs list
                        self.wait_jobs.push(job);
                        debug!("BG_CMD_SUCCESS: Job moved to background successfully");
                    } else {
                        let error_msg = if job_spec.is_empty() {
                            "bg: no stopped jobs".to_string()
                        } else {
                            format!("bg: job not found: {job_spec}")
                        };
                        debug!("BG_CMD_NOT_FOUND: {}", error_msg);
                        ctx.write_stderr(&error_msg)?;
                        return Err(anyhow::anyhow!(error_msg));
                    }
                }
            }
            "reload" => {
                match self.lisp_engine.borrow().run_config_lisp() {
                    Ok(_) => {
                        // Get the config file path for the success message
                        match crate::environment::get_config_file(crate::lisp::CONFIG_FILE) {
                            Ok(config_path) => {
                                self.reload_mcp_config();
                                ctx.write_stdout(&format!(
                                    "Configuration reloaded successfully from {}",
                                    config_path.display()
                                ))?;
                            }
                            Err(_) => {
                                // Fallback to generic message if path resolution fails
                                self.reload_mcp_config();
                                ctx.write_stdout("Configuration reloaded successfully from ~/.config/dsh/config.lisp")?;
                            }
                        }
                    }
                    Err(err) => {
                        // Format error message based on error type for better user experience
                        let error_msg = format_reload_error(&err);
                        ctx.write_stderr(&error_msg)?;
                        return Err(err);
                    }
                }
            }
            _ => {
                // For other commands, try to execute them as external commands
                // We use std::process::Command because we are in a sync context and cannot call async eval_str
                // Note: This bypasses shell aliases/functions for now, which is a limitation of sync proxy.
                use std::process::Command;
                debug!("Dispatching external command: {} {:?}", cmd, argv);

                // If the command contains shell metacharacters or argv is empty (implies potentially complex cmd string passed as one arg), use sh -c
                // Simple heuristic: if argv is empty AND cmd contains space or pipe, or if cmd contains pipe/redirects.
                // Generally safe-run might pass "curl | sh" as cmd with empty argv.
                let use_shell = argv.is_empty()
                    && (cmd.contains(' ')
                        || cmd.contains('|')
                        || cmd.contains('>')
                        || cmd.contains('&'));

                let status = if use_shell {
                    debug!("Detected complex command, using sh -c");
                    Command::new("sh").arg("-c").arg(cmd).status()
                } else {
                    Command::new(cmd).args(argv).status()
                };

                match status {
                    Ok(status) => {
                        if !status.success() {
                            // We return Err to signal failure to the caller (safe-run)
                            // Since dispatch returns Result<()>, we use Err for non-zero exit status if we want safe-run to know.
                            // However, safe-run might want to return the exact exit code.
                            // But for now, returning Err is the only way to signal "something went wrong".
                            return Err(anyhow::anyhow!("Command exited with status: {}", status));
                        }
                    }
                    Err(e) => {
                        return Err(anyhow::anyhow!("Failed to execute '{}': {}", cmd, e));
                    }
                }
            }
        }
        Ok(())
    }

    fn get_var(&mut self, key: &str) -> Option<String> {
        self.environment.read().get_var(key)
    }

    fn get_lisp_var(&self, key: &str) -> Option<String> {
        let lisp_engine = self.lisp_engine.borrow();
        let env = lisp_engine.env.borrow();
        match env.get(&crate::lisp::Symbol::from(key)) {
            Some(crate::lisp::Value::String(s)) => Some(s.clone()),
            Some(crate::lisp::Value::Int(i)) => Some(i.to_string()),
            _ => None,
        }
    }

    fn set_var(&mut self, key: String, value: String) {
        self.environment.write().variables.insert(key, value);
    }

    fn set_env_var(&mut self, key: String, value: String) {
        if key == "PATH" {
            let mut path_vec = vec![];
            for value in value.split(':') {
                path_vec.push(value.to_string());
            }
            let env_path = path_vec.join(":");
            unsafe { std::env::set_var("PATH", &env_path) };
            debug!("set env {} {}", &key, &env_path);
            self.environment.write().reload_path();
        } else {
            unsafe { std::env::set_var(&key, &value) };
            debug!("set env {} {}", &key, &value);
        }
    }

    fn get_alias(&mut self, name: &str) -> Option<String> {
        debug!("Getting alias for: {}", name);
        self.environment.read().alias.get(name).cloned()
    }

    fn set_alias(&mut self, name: String, command: String) {
        debug!("Setting alias: {} = {}", name, command);
        self.environment.write().alias.insert(name, command);
    }

    fn list_aliases(&mut self) -> std::collections::HashMap<String, String> {
        debug!("Listing all aliases");
        self.environment.read().alias.clone()
    }

    fn add_abbr(&mut self, name: String, expansion: String) {
        debug!("Adding abbreviation: {} = {}", name, expansion);
        self.environment
            .write()
            .abbreviations
            .insert(name, expansion);
    }

    fn remove_abbr(&mut self, name: &str) -> bool {
        debug!("Removing abbreviation: {}", name);
        self.environment
            .write()
            .abbreviations
            .remove(name)
            .is_some()
    }

    fn list_abbrs(&self) -> Vec<(String, String)> {
        debug!("Listing all abbreviations");
        self.environment
            .read()
            .abbreviations
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn get_abbr(&self, name: &str) -> Option<String> {
        debug!("Getting abbreviation for: {}", name);
        self.environment.read().abbreviations.get(name).cloned()
    }

    fn list_mcp_servers(&mut self) -> Vec<McpServerConfig> {
        self.environment.read().mcp_servers.clone()
    }

    fn list_execute_allowlist(&mut self) -> Vec<String> {
        self.environment.read().execute_allowlist.clone()
    }

    // New method implementations for export
    fn list_exported_vars(&self) -> Vec<(String, String)> {
        let env = self.environment.read();
        env.exported_vars
            .iter()
            .filter_map(|key| {
                env.variables
                    .get(key)
                    .map(|value| (key.clone(), value.clone()))
            })
            .collect()
    }

    fn export_var(&mut self, key: &str) -> bool {
        let mut env = self.environment.write();
        if env.variables.contains_key(key) {
            env.exported_vars.insert(key.to_string());
            true
        } else {
            // Also allow exporting non-existent variables, they will be exported if set later.
            env.exported_vars.insert(key.to_string());
            false
        }
    }

    fn set_and_export_var(&mut self, key: String, value: String) {
        let mut env = self.environment.write();
        env.variables.insert(key.clone(), value);
        env.exported_vars.insert(key);
    }

    fn get_current_dir(&self) -> Result<std::path::PathBuf> {
        std::env::current_dir().context("failed to get current directory")
    }

    fn confirm_action(&mut self, message: &str) -> Result<bool> {
        use std::io::stdin;

        debug!("Safety confirmation requested: {}", message);

        // Ensure raw mode is enabled to capture a single key press if possible,
        // but since we might be in TUI or plain CLI, we need to be careful.
        // For simplicity and robustness, let's use a blocking read.

        // Use eprint! instead of println! or print! to ensure the prompt goes to stderr.
        // This is critical if the shell output is being piped.
        eprint!("{} [y/N]: ", message);
        use std::io::Write;
        std::io::stderr().flush()?;

        let mut input = String::new();
        stdin().read_line(&mut input)?;

        let confirmed = input.trim().to_lowercase() == "y";
        debug!("Confirmation result: {}", confirmed);
        Ok(confirmed)
    }

    fn is_canceled(&self) -> bool {
        crate::process::signal::check_and_clear_sigint()
    }

    fn get_full_output_history(&self) -> Vec<dsh_types::output_history::OutputEntry> {
        self.environment.read().output_history.get_all_entries()
    }

    fn capture_command(&mut self, _ctx: &Context, cmd: &str) -> Result<(i32, String, String)> {
        use std::process::{Command, Stdio};

        // We implement this synchronously to avoid 'Cannot start a runtime from within a runtime' panic.
        // We replicate the logic of execute_with_capture but for the whole command string
        // passed from safe-run.
        debug!("Capturing command: '{}'", cmd);

        // Use sh -c to execute the command
        let output = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("Failed to capture command: {}", cmd))?;

        let exit_code = output.status.code().unwrap_or(1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        Ok((exit_code, stdout, stderr))
    }

    fn open_editor(&mut self, content: &str, extension: &str) -> Result<String> {
        crate::utils::editor::open_editor(content, extension)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_reload_error_file_not_found() {
        let err = anyhow::anyhow!(
            "Failed to read config file: ~/.config/dsh/config.lisp: No such file or directory"
        );
        let formatted = format_reload_error(&err);
        assert_eq!(
            formatted,
            "reload: file not found: ~/.config/dsh/config.lisp"
        );
    }

    #[test]
    fn test_format_reload_error_permission_denied() {
        let err = anyhow::anyhow!("Permission denied");
        let formatted = format_reload_error(&err);
        assert_eq!(
            formatted,
            "reload: permission denied: cannot read ~/.config/dsh/config.lisp"
        );
    }

    #[test]
    fn test_format_reload_error_xdg_directory() {
        let err = anyhow::anyhow!("failed get xdg directory");
        let formatted = format_reload_error(&err);
        assert_eq!(
            formatted,
            "reload: configuration directory error: unable to access ~/.config/dsh/"
        );
    }

    #[test]
    fn test_format_reload_error_parse_error() {
        let err = anyhow::anyhow!("Parse error: unexpected token ')' at index 15");
        let formatted = format_reload_error(&err);
        assert_eq!(
            formatted,
            "reload: syntax error: unexpected token ')' at index 15"
        );
    }

    #[test]
    fn test_format_reload_error_runtime_error() {
        let err = anyhow::anyhow!("Runtime error: undefined function 'invalid-func'");
        let formatted = format_reload_error(&err);
        assert_eq!(
            formatted,
            "reload: runtime error: undefined function 'invalid-func'"
        );
    }

    #[test]
    fn test_format_reload_error_generic() {
        let err = anyhow::anyhow!("some generic error");
        let formatted = format_reload_error(&err);
        assert_eq!(formatted, "reload: some generic error");
    }

    #[test]
    fn test_parse_z_args() {
        // z -i
        let args = vec!["z".to_string(), "-i".to_string()];
        let (interactive, query) = parse_z_args(&args);
        assert!(interactive);
        assert_eq!(query, "");

        // z --interactive
        let args = vec!["z".to_string(), "--interactive".to_string()];
        let (interactive, query) = parse_z_args(&args);
        assert!(interactive);
        assert_eq!(query, "");

        // z foo
        let args = vec!["z".to_string(), "foo".to_string()];
        let (interactive, query) = parse_z_args(&args);
        assert!(!interactive);
        assert_eq!(query, "foo");

        // z -i foo
        let args = vec!["z".to_string(), "-i".to_string(), "foo".to_string()];
        let (interactive, query) = parse_z_args(&args);
        assert!(interactive);
        assert_eq!(query, "foo");

        // z foo -i
        let args = vec!["z".to_string(), "foo".to_string(), "-i".to_string()];
        let (interactive, query) = parse_z_args(&args);
        assert!(interactive);
        assert_eq!(query, "foo");

        // z foo bar (only first arg taken as query)
        let args = vec!["z".to_string(), "foo".to_string(), "bar".to_string()];
        let (interactive, query) = parse_z_args(&args);
        assert!(!interactive);
        assert_eq!(query, "foo");
    }
}
