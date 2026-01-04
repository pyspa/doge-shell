use crate::ShellProxy;
use anyhow::Result;
use dsh_types::Context;
use getopts::Options;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::borrow::Cow;
use std::sync::Arc;
use sysinfo::System;

pub const COMMAND_NAME: &str = "kill";

pub fn description() -> &'static str {
    "Terminates processes"
}

pub fn command(
    ctx: &Context,
    args: Vec<String>,
    proxy: &mut dyn ShellProxy,
) -> dsh_types::ExitStatus {
    match run(proxy, ctx, COMMAND_NAME, args) {
        Ok(code) => dsh_types::ExitStatus::ExitedWith(code),
        Err(e) => {
            let _ = ctx.write_stderr(&format!("{}: {}\n", COMMAND_NAME, e));
            dsh_types::ExitStatus::ExitedWith(1)
        }
    }
}

struct ProcessItem {
    pid: u32,
    name: String,
    cpu_usage: f32,
    memory: u64,
    cmd: Vec<String>,
}

impl SkimItem for ProcessItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Owned(format!(
            "{:<8} {:<20} {:>6.1}% {:>8} MB  {}",
            self.pid,
            self.name,
            self.cpu_usage,
            self.memory / 1024 / 1024,
            self.cmd.join(" ")
        ))
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Owned(self.pid.to_string())
    }
}

pub fn run(
    _proxy: &mut dyn ShellProxy,
    ctx: &Context,
    cmd: &str,
    args: Vec<String>,
) -> Result<i32> {
    let mut opts = Options::new();
    opts.optflag("h", "help", "print this help menu");
    opts.optopt("s", "signal", "specify signal to send", "SIGNAL");

    let matches = match opts.parse(&args) {
        Ok(m) => m,
        Err(f) => {
            let msg = format!("{}: {}\n", cmd, f);
            let _ = ctx.write_stderr(&msg);
            return Ok(1);
        }
    };

    if matches.opt_present("h") {
        let brief = format!("Usage: {} [options] [PID...]", cmd);
        let _ = ctx.write_stdout(&opts.usage(&brief));
        return Ok(0);
    }

    let signal = if let Some(sig_str) = matches.opt_str("s") {
        match sig_str.parse::<i32>() {
            Ok(sig_num) => match Signal::try_from(sig_num) {
                Ok(s) => s,
                Err(_) => {
                    let _ =
                        ctx.write_stderr(&format!("{}: invalid signal number: {}\n", cmd, sig_num));
                    return Ok(1);
                }
            },
            Err(_) => match sig_str.to_uppercase().as_str() {
                "HUP" => Signal::SIGHUP,
                "INT" => Signal::SIGINT,
                "QUIT" => Signal::SIGQUIT,
                "KILL" => Signal::SIGKILL,
                "TERM" => Signal::SIGTERM,
                "STOP" => Signal::SIGSTOP,
                "CONT" => Signal::SIGCONT,
                _ => {
                    let _ =
                        ctx.write_stderr(&format!("{}: invalid signal name: {}\n", cmd, sig_str));
                    return Ok(1);
                }
            },
        }
    } else {
        Signal::SIGTERM
    };

    if !matches.free.is_empty() {
        // Standard kill behavior: kill PIDs
        let mut exit_code = 0;
        for pid_str in &matches.free {
            match pid_str.parse::<i32>() {
                Ok(pid_num) => {
                    let pid = Pid::from_raw(pid_num);
                    if let Err(e) = signal::kill(pid, signal) {
                        let _ = ctx.write_stderr(&format!(
                            "{}: failed to send signal to {}: {}\n",
                            cmd, pid_num, e
                        ));
                        exit_code = 1;
                    }
                }
                Err(_) => {
                    let _ = ctx.write_stderr(&format!("{}: invalid PID: {}\n", cmd, pid_str));
                    exit_code = 1;
                }
            }
        }
        return Ok(exit_code);
    }

    // Interactive mode (no args)
    let mut sys = System::new();
    sys.refresh_all();

    let mut items: Vec<ProcessItem> = sys
        .processes()
        .iter()
        .map(|(pid, process)| ProcessItem {
            pid: pid.as_u32(),
            name: process.name().to_string_lossy().to_string(),
            cpu_usage: process.cpu_usage(),
            memory: process.memory(),
            // sysinfo cmd() might return &[String] or &[OsString] depending on version/OS
            // Using logic compatible with OsString just in case, based on error.
            cmd: process
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect(),
        })
        .collect();

    // Sort by PID descending (newest first)
    items.sort_by(|a, b| b.pid.cmp(&a.pid));

    let options = SkimOptionsBuilder::default()
        .height("50%".to_string())
        .multi(true)
        .reverse(true)
        .header(Some(
            "Select processes to kill (TAB to select multiple, ENTER to kill)".to_string(),
        ))
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

    let skim_items: Vec<Arc<dyn SkimItem>> = items
        .into_iter()
        .map(|i| Arc::new(i) as Arc<dyn SkimItem>)
        .collect();

    // Run skim
    let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
    for item in skim_items {
        let _ = tx.send(item);
    }
    drop(tx);

    let selected_items = Skim::run_with(&options, Some(rx))
        .map(|out| out.selected_items)
        .unwrap_or_default();

    if selected_items.is_empty() {
        return Ok(0);
    }

    let mut exit_code = 0;
    for item in selected_items {
        let pid_str = item.output();
        if let Ok(pid_num) = pid_str.parse::<i32>() {
            let pid = Pid::from_raw(pid_num);
            // Inform user
            let text = item.text();
            // Extract name for display
            // format is "PID NAME CPU...", so split by space
            let parts: Vec<&str> = text.split_whitespace().collect();
            let name = parts.get(1).unwrap_or(&"???");

            let msg = format!(
                "Killing process {} ({}) with signal {:?}\n",
                pid_num, name, signal
            );
            let _ = ctx.write_stdout(&msg);

            if let Err(e) = signal::kill(pid, signal) {
                let _ = ctx.write_stderr(&format!("{}: failed to kill {}: {}\n", cmd, pid_num, e));
                exit_code = 1;
            }
        }
    }

    Ok(exit_code)
}
