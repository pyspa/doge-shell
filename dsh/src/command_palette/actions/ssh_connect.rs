use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use skim::prelude::*;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub struct SshConnectAction;

impl Action for SshConnectAction {
    fn name(&self) -> &str {
        "SSH Connect"
    }
    fn description(&self) -> &str {
        "Connect to SSH host from ~/.ssh/config"
    }
    fn icon(&self) -> &str {
        "🌐"
    }

    fn execute(&self, _shell: &mut Shell, _input: &str) -> Result<()> {
        // Parse ~/.ssh/config for Host entries
        let config_path = dirs::home_dir()
            .map(|h| h.join(".ssh/config"))
            .unwrap_or_else(|| PathBuf::from("~/.ssh/config"));

        let hosts = parse_ssh_config(&config_path)?;

        if hosts.is_empty() {
            println!("No SSH hosts found in ~/.ssh/config");
            return Ok(());
        }

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt(Some("SSH> "))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for host in hosts {
            let _ = tx.send(Arc::new(host));
        }
        drop(tx);

        let selected = Skim::run_with(&options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let host = item.output().to_string();
            println!("Connecting to {}...", host);

            Command::new("ssh")
                .arg(&host)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to connect: {}", e))?;
        }

        Ok(())
    }
}

fn parse_ssh_config(path: &PathBuf) -> Result<Vec<String>> {
    let content = fs::read_to_string(path).unwrap_or_default();
    let mut hosts = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.to_lowercase().starts_with("host ") {
            let host_part = &line[5..].trim();
            // Skip wildcards
            if !host_part.contains('*') && !host_part.contains('?') {
                // May have multiple hosts on one line
                for host in host_part.split_whitespace() {
                    if !host.contains('*') && !host.contains('?') {
                        hosts.push(host.to_string());
                    }
                }
            }
        }
    }

    Ok(hosts)
}
