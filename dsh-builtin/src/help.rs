use super::{ShellProxy, get_all_commands};
use dsh_types::{Context, ExitStatus};

/// Built-in help command implementation
/// Displays a list of all available built-in commands with their descriptions
pub fn command(ctx: &Context, _argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Get all commands with their descriptions from the registry
    let commands = get_all_commands();

    // Format and display the help information
    let mut help_text = String::from("Built-in commands:\n");
    help_text.push('\n');

    for (cmd, description) in commands {
        help_text.push_str(&format!("{:<12} {}\n", cmd, description));
    }

    match ctx.write_stdout(&help_text) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(err) => {
            ctx.write_stderr(&format!("help: failed to display help: {err}"))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}
