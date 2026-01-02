use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use crossterm::style::Stylize;
use std::io::Write;
use std::process::{Command, Stdio};

pub struct DashboardAction;

impl Action for DashboardAction {
    fn name(&self) -> &str {
        "Dashboard"
    }
    fn description(&self) -> &str {
        "Show project dashboard"
    }
    fn execute(&self, _shell: &mut Shell) -> Result<()> {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());

        // Get git branch
        let branch = Command::new("git")
            .args(["branch", "--show-current"])
            .stdout(Stdio::piped())
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "N/A".to_string());

        // Get git status summary
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .stdout(Stdio::piped())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
            .unwrap_or(0);

        let width = 50;
        let border = "═".repeat(width);

        println!("╔{}╗", border);
        println!(
            "║ {:^width$} ║",
            "🐕 doge-shell dashboard".bold().yellow(),
            width = width
        );
        println!("╠{}╣", border);
        println!("║ {:width$} ║", format!("📁 {}", cwd).cyan(), width = width);
        println!(
            "║ {:width$} ║",
            format!("🌿 Branch: {}", branch).green(),
            width = width
        );
        println!(
            "║ {:width$} ║",
            format!("📝 Changes: {} files", status),
            width = width
        );
        println!("╚{}╝", border);

        std::io::stdout().flush()?;
        Ok(())
    }
}
