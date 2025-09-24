use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use uuid::Uuid;

/// Built-in uuid command description
pub fn description() -> &'static str {
    "Generate a random UUID"
}

/// Built-in uuid command implementation
/// Generates and outputs a random UUID (Universally Unique Identifier)
/// Uses UUID version 4 (random) for maximum uniqueness
pub fn command(ctx: &Context, _args: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Generate a new random UUID (version 4)
    let id = Uuid::new_v4();

    // Output the UUID string to stdout
    match ctx.write_stdout(&id.to_string()) {
        Err(err) => {
            // Handle output errors gracefully
            let _ = ctx.write_stderr(&format!("uuid: {err}")); // TODO err check
            ExitStatus::ExitedWith(1)
        }
        _ => ExitStatus::ExitedWith(0),
    }
}
