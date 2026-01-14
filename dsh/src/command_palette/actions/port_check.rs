use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use skim::prelude::*;
use std::process::{Command, Stdio};

pub struct PortCheckAction;

impl Action for PortCheckAction {
    fn name(&self) -> &str {
        "Port Check"
    }
    fn description(&self) -> &str {
        "Check ports in use and kill process"
    }
    fn icon(&self) -> &str {
        "🔌"
    }

    fn category(&self) -> &str {
        "System"
    }
    fn execute(&self, _shell: &mut Shell, _input: &str) -> Result<()> {
        // Get listening ports using lsof or ss
        let output = Command::new("lsof")
            .args(["-i", "-P", "-n"])
            .stdout(Stdio::piped())
            .output()
            .or_else(|_| {
                Command::new("ss")
                    .args(["-tulpn"])
                    .stdout(Stdio::piped())
                    .output()
            })?;

        if !output.status.success() {
            println!("Could not get port information");
            return Ok(());
        }

        let ports = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = ports.lines().skip(1).collect(); // Skip header

        if lines.is_empty() {
            println!("No listening ports found");
            return Ok(());
        }

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt("Port> ".to_string())
            .header(Some(ports.lines().next().unwrap_or("").to_string()))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for line in &lines {
            let _ = tx.send(Arc::new(line.to_string()));
        }
        drop(tx);

        let selected = Skim::run_with(&options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let line = item.output().to_string();

            // Try to extract PID from lsof output (format: COMMAND PID ...)
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let pid = parts[1];

                // Confirm kill
                println!("Kill process {} (PID: {})? [y/N]", parts[0], pid);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;

                if input.trim().to_lowercase() == "y" {
                    Command::new("kill")
                        .arg(pid)
                        .status()
                        .map_err(|e| anyhow::anyhow!("Failed to kill: {}", e))?;
                    println!("Process {} killed", pid);
                }
            }
        }

        Ok(())
    }
}
