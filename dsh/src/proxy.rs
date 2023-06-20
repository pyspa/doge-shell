use crate::shell::Shell;
use anyhow::Result;
use dsh_builtin::ShellProxy;
use dsh_frecency::SortMethod;
use dsh_types::Context;
use nix::unistd::dup;
use std::fs::File;
use std::io::prelude::*;
use std::io::Write;
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
                    let fd = dup(ctx.outfile).expect("failed dup");
                    let mut file = unsafe { File::from_raw_fd(fd) };
                    let vec = history.sorted(&SortMethod::Recent);
                    for item in vec {
                        writeln!(file, "{}", item.item).ok();
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
                    self.print_stdout("jobs: There are no jobs");
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
                    self.print_stdout(table.as_str());
                }
            }
            "lisp" => match self.lisp_engine.borrow().run(argv[1].as_str()) {
                Ok(val) => {
                    debug!("{}", val);
                }
                Err(err) => {
                    eprintln!("{}", err);
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
                        eprintln!("{}", err);
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
                self.print_stdout(table.as_str());
            }
            "read" => {
                let mut stdin = Vec::new();
                unsafe { File::from_raw_fd(ctx.infile).read_to_end(&mut stdin).ok() };
                let key = format!("${}", argv[1]);
                let output = std::str::from_utf8(&stdin)
                    .map_err(|err| {
                        // TODO
                        eprintln!("{:?}", err);
                        err
                    })
                    .unwrap()
                    .trim_end_matches('\n')
                    .to_owned();

                self.environment.write().variables.insert(key, output);
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
            std::env::set_var("PATH", &env_path);
            debug!("set env {} {}", &key, &env_path);
            self.environment.write().reload_path();
        } else {
            std::env::set_var(&key, &value);
            debug!("set env {} {}", &key, &value);
        }
    }
}
