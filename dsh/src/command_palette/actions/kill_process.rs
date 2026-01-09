use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use skim::prelude::*;
use std::process::{Command, Stdio};

pub struct KillProcessAction;

impl Action for KillProcessAction {
    fn name(&self) -> &str {
        "Kill Process"
    }
    fn description(&self) -> &str {
        "Kill a running process"
    }
    fn icon(&self) -> &str {
        "💀"
    }

    fn execute(&self, _shell: &mut Shell) -> Result<()> {
        // Get process list
        let output = Command::new("ps")
            .args(["aux"])
            .stdout(Stdio::piped())
            .output()?;

        if !output.status.success() {
            return Err(anyhow::anyhow!("Failed to list processes"));
        }

        let processes = String::from_utf8_lossy(&output.stdout);
        let process_list: Vec<&str> = processes.lines().skip(1).collect(); // Skip header

        if process_list.is_empty() {
            println!("No processes found");
            return Ok(());
        }

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt("Process> ".to_string())
            .header(Some(
                "USER       PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND"
                    .to_string(),
            ))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for process in process_list {
            let _ = tx.send(Arc::new(process.to_string()));
        }
        drop(tx);

        let selected = Skim::run_with(&options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let line = item.output().to_string();
            // Parse PID (second field)
            let pid = line.split_whitespace().nth(1);

            if let Some(pid) = pid {
                println!("Killing process {}", pid);
                Command::new("kill")
                    .arg(pid)
                    .status()
                    .map_err(|e| anyhow::anyhow!("Failed to kill process: {}", e))?;
            }
        }

        Ok(())
    }
}
