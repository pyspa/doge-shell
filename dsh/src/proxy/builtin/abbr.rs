use crate::shell::Shell;
use anyhow::{Result, bail};
use dsh_types::Context;

pub fn execute(shell: &mut Shell, _ctx: &Context, argv: Vec<String>) -> Result<()> {
    if argv.len() < 4 {
        bail!("abbr-command requires command, name, and expansion");
    }
    let command = argv[1].clone();
    let name = argv[2].clone();
    let expansion = argv[3..].join(" ");
    shell
        .environment
        .write()
        .variable_state
        .command_abbreviations
        .entry(command)
        .or_default()
        .insert(name, expansion);
    Ok(())
}
