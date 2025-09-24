use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in help command implementation
/// Displays a list of all available built-in commands with their descriptions
pub fn command(ctx: &Context, _argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Define descriptions for each built-in command
    let commands = vec![
        ("abbr", "Manage abbreviations that expand when typed"),
        ("add_path", "Add paths to the PATH environment variable"),
        ("alias", "Create and manage command aliases"),
        ("bg", "Resume a stopped job in the background"),
        ("cd", "Change the current working directory"),
        ("chat", "Chat with AI assistant"),
        ("chat_model", "Set or show the AI model used for chat"),
        ("chat_prompt", "Set or show the system prompt for chat"),
        ("dmv", "Rename files with your editor"),
        ("exit", "Exit the shell"),
        ("fg", "Resume a stopped job in the foreground"),
        ("gco", "Checkout git branches with fzf selection"),
        ("glog", "View git log with fzf selection"),
        ("history", "Show command history"),
        ("jobs", "List active jobs"),
        ("lisp", "Execute Lisp code"),
        ("read", "Read a line from standard input"),
        ("reload", "Reload shell configuration"),
        ("serve", "Start a simple HTTP file server"),
        ("set", "Set shell options"),
        ("uuid", "Generate a random UUID"),
        ("var", "Manage shell variables"),
        ("z", "Jump to frequently used directories"),
    ];

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
