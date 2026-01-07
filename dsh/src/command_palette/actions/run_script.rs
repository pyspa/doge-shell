use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use skim::prelude::*;
use std::fs;
use std::process::Command;

pub struct RunScriptAction;

impl Action for RunScriptAction {
    fn name(&self) -> &str {
        "Run Script"
    }
    fn description(&self) -> &str {
        "Run npm/make/cargo script"
    }
    fn category(&self) -> &str {
        "Dev"
    }
    fn execute(&self, _shell: &mut Shell) -> Result<()> {
        let mut scripts: Vec<(String, String)> = Vec::new(); // (display, command)

        // Check package.json
        if let Ok(content) = fs::read_to_string("package.json")
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
            && let Some(obj) = json.get("scripts").and_then(|s| s.as_object())
        {
            for (name, _) in obj {
                scripts.push((format!("[npm] {}", name), format!("npm run {}", name)));
            }
        }

        // Check Makefile
        if fs::metadata("Makefile").is_ok() || fs::metadata("makefile").is_ok() {
            let output = Command::new("make").args(["-pRrq", ":"]).output().ok();

            if let Some(out) = output {
                let content = String::from_utf8_lossy(&out.stdout);
                for line in content.lines() {
                    if line.ends_with(':') && !line.starts_with('.') && !line.starts_with('#') {
                        let target = line.trim_end_matches(':');
                        if !target.is_empty() && !target.contains(' ') {
                            scripts
                                .push((format!("[make] {}", target), format!("make {}", target)));
                        }
                    }
                }
            }
        }

        // Check Cargo.toml for common cargo commands
        if fs::metadata("Cargo.toml").is_ok() {
            scripts.push(("[cargo] build".to_string(), "cargo build".to_string()));
            scripts.push(("[cargo] run".to_string(), "cargo run".to_string()));
            scripts.push(("[cargo] test".to_string(), "cargo test".to_string()));
            scripts.push(("[cargo] check".to_string(), "cargo check".to_string()));
            scripts.push(("[cargo] clippy".to_string(), "cargo clippy".to_string()));
        }

        if scripts.is_empty() {
            println!("No scripts found (package.json, Makefile, or Cargo.toml)");
            return Ok(());
        }

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt("Script> ".to_string())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for (display, _) in &scripts {
            let _ = tx.send(Arc::new(display.clone()));
        }
        drop(tx);

        let selected = Skim::run_with(&options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let display = item.output().to_string();
            // Find the corresponding command
            if let Some((_, cmd)) = scripts.iter().find(|(d, _)| d == &display) {
                println!("$ {}", cmd);
                let parts: Vec<&str> = cmd.split_whitespace().collect();
                if !parts.is_empty() {
                    Command::new(parts[0])
                        .args(&parts[1..])
                        .status()
                        .map_err(|e| anyhow::anyhow!("Failed to run script: {}", e))?;
                }
            }
        }

        Ok(())
    }
}
