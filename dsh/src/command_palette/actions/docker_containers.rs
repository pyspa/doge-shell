use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;
use skim::prelude::*;
use std::process::{Command, Stdio};

pub struct DockerContainersAction;

#[async_trait(?Send)]
impl Action for DockerContainersAction {
    fn name(&self) -> &str {
        "Docker Containers"
    }
    fn description(&self) -> &str {
        "Manage Docker containers"
    }
    fn icon(&self) -> &str {
        "🐳"
    }

    async fn execute(&self, _shell: &mut Shell, _input: &str) -> Result<()> {
        // Get container list (all, including stopped)
        let output = Command::new("docker")
            .args([
                "ps",
                "-a",
                "--format",
                "{{.Names}}\t{{.Status}}\t{{.Image}}",
            ])
            .stdout(Stdio::piped())
            .output();

        let output = match output {
            Ok(o) if o.status.success() => o,
            _ => {
                println!("Docker is not available or no containers found");
                return Ok(());
            }
        };

        let containers = String::from_utf8_lossy(&output.stdout);
        let container_list: Vec<&str> = containers.lines().collect();

        if container_list.is_empty() {
            println!("No Docker containers found");
            return Ok(());
        }

        use crate::command_palette::StringItem;

        // Select a container
        let container_options = SkimOptionsBuilder::default()
            .prompt("Container> ".to_string())
            .header("NAME\tSTATUS\tIMAGE".to_string())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for container in container_list {
            let _ = tx.send(vec![Arc::new(StringItem(container.to_string()))]);
        }
        drop(tx);

        let selected = crate::utils::skim::run_skim_with(container_options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if selected.is_empty() {
            return Ok(());
        }

        let container_line = selected[0].output().to_string();
        let container_name = container_line.split('\t').next().unwrap_or("").to_string();

        if container_name.is_empty() {
            return Ok(());
        }

        // Select an action
        let actions = vec![
            "start",
            "stop",
            "restart",
            "logs",
            "logs -f",
            "exec -it bash",
            "exec -it sh",
            "rm",
        ];
        let action_options = SkimOptionsBuilder::default()
            .prompt("Action> ".to_string()) // Changed from Some("Action> ") to "Action> ".to_string()
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        // Prepare items for batch sending
        let action_items: Vec<Arc<dyn SkimItem>> = actions
            .into_iter()
            .map(|s| Arc::new(s.to_string()) as Arc<dyn SkimItem>)
            .collect();
        let _ = tx.send(action_items); // Batch send
        drop(tx);

        let selected_action = crate::utils::skim::run_skim_with(action_options, Some(rx)) // `action_options` is now owned
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(action_item) = selected_action.first() {
            let action = action_item.output().to_string();
            let args: Vec<&str> = action.split_whitespace().collect();

            println!("docker {} {}", action, container_name);

            let mut cmd = Command::new("docker");
            cmd.args(&args);
            cmd.arg(&container_name);
            cmd.status()
                .map_err(|e| anyhow::anyhow!("Failed to execute docker {}: {}", action, e))?;
        }

        Ok(())
    }
}
