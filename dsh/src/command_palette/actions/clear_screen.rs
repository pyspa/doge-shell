use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;

pub struct ClearScreenAction;
impl Action for ClearScreenAction {
    fn name(&self) -> &str {
        "Clear Screen"
    }
    fn description(&self) -> &str {
        "Clear the terminal screen"
    }
    fn category(&self) -> &str {
        "Shell"
    }
    fn execute(&self, _shell: &mut Shell) -> Result<()> {
        print!("\x1B[2J\x1B[1;1H");
        std::io::Write::flush(&mut std::io::stdout())?;
        Ok(())
    }
}
