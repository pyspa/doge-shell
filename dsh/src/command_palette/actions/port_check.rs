use super::super::Action;
use crate::shell::Shell;
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use skim::prelude::*;
use std::process::Stdio;

pub struct PortCheckAction;

#[async_trait(?Send)]
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
    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        let runtime = super::runtime_snapshot(shell);
        // Get listening ports using lsof or ss, resolved through the
        // logical runtime. A missing tool falls through to the next one.
        let run_list = |program: &str, args: &[&str]| -> Option<std::process::Output> {
            runtime
                .std_command(program)?
                .args(args)
                .stdout(Stdio::piped())
                .output()
                .ok()
        };
        let output = run_list("lsof", &["-i", "-P", "-n"])
            .or_else(|| run_list("ss", &["-tulpn"]))
            .context("command not found: lsof/ss")?;

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

        use crate::command_palette::StringItem;

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt("Port> ".to_string())
            .header("PROTO\tLOCAL\tREMOTE\tSTATUS\tPID/PROGRAM".to_string())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for port_line in lines {
            let _ = tx.send(vec![Arc::new(StringItem(port_line.to_string()))]);
        }
        drop(tx);

        let selected = crate::utils::skim::run_skim_with(options, Some(rx))
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
                    runtime
                        .std_command("kill")
                        .context("command not found: kill")?
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
