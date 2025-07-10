use crate::{process::ProcessState, shell::Shell};
use anyhow::Result;
use dsh_builtin::ShellProxy;
use dsh_frecency::SortMethod;
use dsh_types::Context;
use std::fs::File;
use std::io::prelude::*;
use std::os::unix::io::FromRawFd;
use tabled::{Table, Tabled};
use tracing::debug;

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

impl ShellProxy for Shell {
    fn exit_shell(&mut self) {
        self.exit();
    }

    fn save_path_history(&mut self, path: &str) {
        if let Some(ref mut history) = self.path_history {
            let mut history = history.lock().unwrap();
            history.add(path);
        }
    }

    fn changepwd(&mut self, path: &str) -> Result<()> {
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
                    let mut history = history.lock().unwrap();
                    let vec = history.sorted(&SortMethod::Recent);
                    for item in vec {
                        ctx.write_stdout(&item.item)?;
                    }
                    history.reset_index();
                }
            }
            "z" => {
                let path = argv.get(1).map(|s| s.as_str()).unwrap_or("");
                let path = if let Some(ref mut history) = self.path_history {
                    let history = history.clone();
                    let history = history.lock().unwrap();
                    let results = history.sort_by_match(path);
                    if !results.is_empty() {
                        let path = results[0].item.to_string();
                        Some(path)
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
            "jobs" => {
                if self.wait_jobs.is_empty() {
                    ctx.write_stdout("jobs: there are no jobs")?;
                } else {
                    let jobs: Vec<Job> = self
                        .wait_jobs
                        .iter()
                        .map(|job| Job {
                            job: job.job_id,
                            pid: job.pid.unwrap().as_raw(),
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
                    ctx.write_stderr(&format!("{}", err))?;
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
                        ctx.write_stderr(&format!("{}", err))?;
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
                unsafe { File::from_raw_fd(ctx.infile).read_to_end(&mut stdin).ok() };
                let key = format!("${}", argv[1]);
                let output = match std::str::from_utf8(&stdin) {
                    Ok(s) => s.trim_end_matches('\n').to_owned(),
                    Err(err) => {
                        ctx.write_stderr(&format!("read: invalid UTF-8 input: {}", err))
                            .ok();
                        return Err(anyhow::anyhow!("invalid UTF-8 input: {}", err));
                    }
                };

                self.environment.write().variables.insert(key, output);
            }
            "fg" => {
                debug!("call fg - wait_jobs.len(): {}", self.wait_jobs.len());
                if self.wait_jobs.is_empty() {
                    ctx.write_stdout("fg: there are no suitable jobs")?;
                } else {
                    // Parse job specification from arguments
                    let job_spec = argv.get(1).map(|s| s.as_str()).unwrap_or("");

                    if let Some(job_index) = parse_job_spec(job_spec, &self.wait_jobs) {
                        let mut job = self.wait_jobs.remove(job_index);
                        debug!("foreground job: {:?}", job);
                        debug!("Job state before fg: {:?}", job.state);
                        debug!("Job pgid: {:?}, pid: {:?}", job.pgid, job.pid);

                        ctx.write_stdout(&format!(
                            "dsh: job {} '{}' to foreground",
                            job.job_id, job.cmd
                        ))
                        .ok();

                        let cont = if let ProcessState::Stopped(_, _) = job.state {
                            debug!("Job is stopped, will send SIGCONT");
                            true
                        } else {
                            debug!("Job is not stopped, no SIGCONT needed");
                            false
                        };
                        job.state = ProcessState::Running;
                        debug!("Set job state to Running");

                        debug!(
                            "About to call put_in_foreground_sync with no_hang=true, cont={}",
                            cont
                        );
                        if let Err(err) = job.put_in_foreground_sync(true, cont) {
                            debug!("put_in_foreground_sync failed with error: {:?}", err);
                            ctx.write_stderr(&format!("{}", err)).ok();
                            return Err(err);
                        }
                        debug!("put_in_foreground_sync completed successfully");
                    } else {
                        let error_msg = if job_spec.is_empty() {
                            "fg: no current job".to_string()
                        } else {
                            format!("fg: job not found: {}", job_spec)
                        };
                        ctx.write_stderr(&error_msg)?;
                        return Err(anyhow::anyhow!(error_msg));
                    }
                }
            }
            "bg" => {
                debug!("call bg - wait_jobs.len(): {}", self.wait_jobs.len());
                if self.wait_jobs.is_empty() {
                    ctx.write_stdout("bg: there are no suitable jobs")?;
                } else {
                    // Parse job specification from arguments
                    let job_spec = argv.get(1).map(|s| s.as_str()).unwrap_or("");

                    // Find job by specification or default to most recent stopped job
                    let job_index = if job_spec.is_empty() {
                        // Find the most recent stopped job
                        let mut found_index = None;
                        for (i, job) in self.wait_jobs.iter().enumerate().rev() {
                            if matches!(job.state, ProcessState::Stopped(_, _)) {
                                found_index = Some(i);
                                break;
                            }
                        }
                        found_index
                    } else {
                        // Parse job specification
                        parse_job_spec(job_spec, &self.wait_jobs)
                    };

                    if let Some(index) = job_index {
                        let job = &self.wait_jobs[index];

                        // Check if job is actually stopped
                        if !matches!(job.state, ProcessState::Stopped(_, _)) {
                            let error_msg = format!("bg: job {} is already running", job.job_id);
                            ctx.write_stderr(&error_msg)?;
                            return Err(anyhow::anyhow!(error_msg));
                        }

                        let mut job = self.wait_jobs.remove(index);
                        debug!("background job: {:?}", job);
                        debug!("Job state before bg: {:?}", job.state);
                        debug!("Job pgid: {:?}, pid: {:?}", job.pgid, job.pid);

                        ctx.write_stdout(&format!(
                            "dsh: job {} '{}' to background",
                            job.job_id, job.cmd
                        ))
                        .ok();

                        // Set job state to running and send SIGCONT
                        job.state = ProcessState::Running;
                        debug!("Set job state to Running");

                        // Send SIGCONT to resume the job
                        if let Some(pgid) = job.pgid {
                            debug!("Sending SIGCONT to process group {}", pgid);
                            use nix::sys::signal::{Signal, killpg};
                            if let Err(err) = killpg(pgid, Signal::SIGCONT) {
                                debug!("Failed to send SIGCONT: {}", err);
                                ctx.write_stderr(&format!("bg: failed to resume job: {}", err))
                                    .ok();
                                return Err(err.into());
                            }
                            debug!("SIGCONT sent successfully");
                        }

                        // Put the job back in the background jobs list
                        self.wait_jobs.push(job);
                        debug!("Job moved to background successfully");
                    } else {
                        let error_msg = if job_spec.is_empty() {
                            "bg: no stopped jobs".to_string()
                        } else {
                            format!("bg: job not found: {}", job_spec)
                        };
                        ctx.write_stderr(&error_msg)?;
                        return Err(anyhow::anyhow!(error_msg));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn get_var(&mut self, key: &str) -> Option<String> {
        self.environment.read().get_var(key)
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
}
