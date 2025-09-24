use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in lisp command description
pub fn description() -> &'static str {
    "Execute Lisp code"
}

/// Built-in lisp command implementation
/// Evaluates Lisp s-expressions for shell scripting and configuration
/// Supports the shell's integrated Lisp interpreter for advanced scripting
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        // Require at least one s-expression argument
        ctx.write_stderr("lisp: missing s-expression").ok();
    } else {
        // Delegate Lisp evaluation to the shell's Lisp interpreter
        proxy.dispatch(ctx, "lisp", argv).unwrap();
    }
    ExitStatus::ExitedWith(0)
}

/// Built-in lisp-run command implementation
/// Executes Lisp scripts from files or runs Lisp code in batch mode
/// Provides error handling for Lisp execution failures
pub fn run(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.dispatch(ctx, "lisp-run", argv) {
        Ok(_) => ExitStatus::ExitedWith(0),
        // Return error status if Lisp execution fails
        _ => ExitStatus::ExitedWith(1),
    }
}
