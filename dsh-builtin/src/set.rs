use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use getopts::Options;

/// Built-in set command description
pub fn description() -> &'static str {
    "Set shell options"
}

/// Prints usage information for the set command
/// Displays command syntax and available options
fn print_usage(ctx: &Context, cmd_name: &str, opts: Options) {
    let brief = format!("Usage: {cmd_name} [OPTIONS] KEY VALUE");
    ctx.write_stdout(&opts.usage(&brief)).ok();
}

/// Built-in set command implementation
/// Sets shell variables or environment variables with optional export functionality
/// Supports both local shell variables and exported environment variables
pub fn command(ctx: &Context, args: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let cmd_name = args[0].clone();
    let mut opts = Options::new();

    // Define command-line options
    opts.optflag("x", "export", "exported environment variable");
    opts.optflag("h", "help", "print this help menu");

    // Parse command-line arguments
    let matches = match opts.parse(&args[1..]) {
        Ok(m) => m,
        Err(err) => {
            ctx.write_stderr(&format!("{err}")).ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    // Handle help option or invalid argument count
    if matches.opt_present("h") || matches.free.len() != 2 {
        print_usage(ctx, &cmd_name, opts);
        return ExitStatus::ExitedWith(0);
    }

    let key = matches.free[0].clone();
    let val = matches.free[1].clone();

    if matches.opt_present("x") {
        // Export as environment variable (available to child processes)
        proxy.set_env_var(key, val);
    } else {
        // Set as local shell variable (shell session only)
        proxy.set_var(key, val);
    }
    ExitStatus::ExitedWith(0)
}
