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
    command: String,
}
#[derive(Tabled)]
struct Var {
    key: String,
    value: String,
}

impl ShellProxy for Shell {
    fn save_path_history(&mut self, path: &str) {
        if let Some(ref mut history) = self.path_history {
            history.add(path);
        }
    }

    fn changepwd(&mut self, path: &str) {
        self.save_path_history(path);
        self.chpwd(path);
    }

    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()> {
        match cmd {
            "exit" => {
                self.exit();
            }
            "cd" => {
                let dir = &argv[0];
                self.changepwd(&dir);
            }
            "move_dir" => {
                let dir = &argv[0];
                self.save_path_history(dir);
            }
            "history" => {
                if let Some(ref mut history) = self.cmd_history {
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
                if let Some(ref mut history) = self.path_history {
                    let results = history.sort_by_match(path);
                    if !results.is_empty() {
                        let path = results[0].item.clone();
                        self.dispatch(ctx, "cd", vec![path]).unwrap();
                    }
                }
            }
            "jobs" => {
                let jobs: Vec<Job> = self
                    .wait_jobs
                    .iter()
                    .map(|job| Job {
                        job: job.wait_job_id,
                        pid: job.pid.as_raw(),
                        command: job.cmd.clone(),
                    })
                    .collect();

                let table = Table::new(jobs).to_string();
                self.print_stdout(table);
            }
            "lisp" => match self.lisp_engine.borrow().run(argv[1].as_str()) {
                Ok(val) => {
                    debug!("{}", val);
                }
                Err(err) => {
                    eprintln!("{}", err);
                }
            },
            "set" => {
                let key = format!("${}", &argv[1]);
                let val = &argv[2];
                self.environment
                    .borrow_mut()
                    .variables
                    .insert(key, val.to_string());
            }
            "var" => {
                let vars: Vec<Var> = self
                    .environment
                    .borrow()
                    .variables
                    .iter()
                    .map(|x| Var {
                        key: x.0.to_owned(),
                        value: x.1.to_owned(),
                    })
                    .collect();
                let table = Table::new(vars).to_string();
                self.print_stdout(table);
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

                self.environment.borrow_mut().variables.insert(key, output);
            }
            _ => {}
        }
        Ok(())
    }

    fn get_var(&mut self, key: &str) -> Option<String> {
        if let Some(val) = self.environment.borrow().variables.get(key) {
            Some(val.to_string())
        } else {
            None
        }
    }

    fn save_var(&mut self, key: String, value: String) {
        self.environment.borrow_mut().variables.insert(key, value);
    }
}
