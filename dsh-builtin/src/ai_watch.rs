use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn description() -> &'static str {
    "Watch a command with AI from the interactive REPL"
}

pub fn command(ctx: &Context, argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv
        .iter()
        .any(|arg| arg == "-h" || arg == "--help" || arg == "help")
    {
        let _ = ctx.write_stdout(help_text());
        return ExitStatus::ExitedWith(0);
    }

    let _ = ctx.write_stderr(
        "ai-watch: run this command from the interactive dsh REPL so it can use the normal execution path and command block recording.",
    );
    let _ = ctx.write_stderr(help_text());
    ExitStatus::ExitedWith(1)
}

fn help_text() -> &'static str {
    concat!(
        "Usage: ai-watch [--goal <text>] -- <command>\n",
        "\n",
        "Explicitly watch a command with AI and save the final summary to command blocks.\n",
        "This v1 feature is handled before execution in the interactive REPL.\n",
        "\n",
        "Examples:\n",
        "  ai-watch -- cargo test -p doge-shell\n",
        "  ai-watch --goal \"server ready を検出\" -- npm run dev\n",
    )
}
