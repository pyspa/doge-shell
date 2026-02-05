use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;

pub struct ClearScreenAction;

#[async_trait(?Send)]
impl Action for ClearScreenAction {
    fn name(&self) -> &str {
        "Clear Screen"
    }
    fn description(&self) -> &str {
        "Clear the terminal screen"
    }
    fn icon(&self) -> &str {
        "🧹"
    }

    fn category(&self) -> &str {
        "Shell"
    }
    async fn execute(&self, _shell: &mut Shell, _input: &str) -> Result<()> {
        print!("\x1B[2J\x1B[1;1H");
        std::io::Write::flush(&mut std::io::stdout())?;
        Ok(())
    }
}
