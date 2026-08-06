use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;
use dsh_frecency::SortMethod;
use skim::prelude::*;

pub struct JumpDirectoryAction;

#[async_trait(?Send)]
impl Action for JumpDirectoryAction {
    fn name(&self) -> &str {
        "Jump to Directory"
    }
    fn description(&self) -> &str {
        "Jump to frequently used directory"
    }
    fn icon(&self) -> &str {
        "🚀"
    }

    async fn execute(&self, shell: &mut Shell, _input: &str) -> Result<()> {
        // Get directory history (frecency-based)
        let directories: Vec<String> = if let Some(ref history) = shell.path_history {
            let locked = history.lock();
            locked
                .sorted(&SortMethod::Frecent)
                .into_iter()
                .take(100)
                .map(|item| item.item)
                .collect()
        } else {
            return Err(anyhow::anyhow!("Directory history not available"));
        };

        if directories.is_empty() {
            println!("No directory history");
            return Ok(());
        }

        use crate::command_palette::StringItem;

        // Show selection UI
        let options = SkimOptionsBuilder::default()
            .prompt("Jump> ".to_string())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        for dir in directories {
            let _ = tx.send(vec![Arc::new(StringItem(dir))]);
        }
        drop(tx);

        let selected = crate::utils::skim::run_skim_with(options, Some(rx))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        if let Some(item) = selected.first() {
            let dir_path = item.output().to_string();

            // Go through `changepwd` rather than `set_current_dir`: that is the
            // single place that records `$OLDPWD`, feeds directory frecency,
            // runs `*on-chdir-hooks*` and keeps slot 0 of the `pushd` stack in
            // step with the real cwd. Moving the shell behind its back leaves
            // `dirs` describing a directory we are no longer in.
            use dsh_builtin::ShellProxy;
            shell
                .changepwd(&dir_path)
                .map_err(|e| anyhow::anyhow!("Failed to change directory: {}", e))?;

            println!("cd {}", dir_path);
        }

        Ok(())
    }
}
