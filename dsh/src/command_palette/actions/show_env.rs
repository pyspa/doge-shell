use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;
use skim::prelude::*;

pub struct ShowEnvAction;

#[async_trait(?Send)]
impl Action for ShowEnvAction {
    fn name(&self) -> &str {
        "Show Environment"
    }
    fn description(&self) -> &str {
        "Search and display environment variables"
    }
    fn icon(&self) -> &str {
        "🌿"
    }

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        // Collect environment variables
        let mut env_vars: Vec<String> = Vec::new();

        // From shell environment
        {
            let env = shell.environment.read();
            for (key, value) in &env.variables {
                env_vars.push(format!("{}={}", key, value));
            }
        }

        // From system environment
        for (key, value) in std::env::vars() {
            let entry = format!("{}={}", key, value);
            if !env_vars.contains(&entry) {
                env_vars.push(entry);
            }
        }

        env_vars.sort();

        if env_vars.is_empty() {
            println!("No environment variables");
            return Ok(());
        }

        use crate::command_palette::StringItem;

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt("Env> ".to_string())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for var in env_vars {
            let _ = tx.send(vec![Arc::new(StringItem(var))]);
        }
        drop(tx);

        let selected = crate::utils::skim::run_skim_with(options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let var = item.output().to_string();
            println!("{}", var);
        }

        Ok(())
    }
}
