pub mod config;
pub mod error;
pub mod handlers;
pub mod scanner;
pub mod server;

use crate::ShellProxy;
use config::parse_arguments;
use dsh_types::{Context, ExitStatus};
use error::ServeError;
use server::start_http_server;
use tracing::debug;

/// Built-in serve command description
pub fn description() -> &'static str {
    "Start a simple HTTP file server"
}

/// Main serve command entry point
/// Implements the builtin command interface for the HTTP serve functionality
pub fn command(ctx: &Context, argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    debug!("serve command called with args: {:?}", argv);

    // Parse command-line arguments with enhanced error handling
    let config = match parse_arguments(&argv) {
        Ok(config) => config,
        Err(err) => {
            match &err {
                ServeError::ArgumentError(msg) => {
                    // Check if this is a help request (not an error)
                    if msg.contains("Usage:") {
                        ctx.write_stdout(msg).ok();
                        return ExitStatus::ExitedWith(0);
                    } else {
                        ctx.write_stderr(&format!("serve: {msg}")).ok();
                        return ExitStatus::ExitedWith(1);
                    }
                }
                _ => {
                    return handle_config_error(&err, ctx);
                }
            }
        }
    };

    debug!("serve config: {:?}", config);

    // Start the HTTP server
    match start_http_server(ctx, config) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(err) => handle_config_error(&err, ctx),
    }
}

/// Handle port conflict by suggesting alternatives
fn handle_port_conflict(port: u16, ctx: &Context) -> ExitStatus {
    let alternatives = ServeError::suggest_alternative_ports(port);
    let error = ServeError::PortInUse { port };

    ctx.write_stderr(&format!("serve: {}", error.user_message()))
        .ok();

    if !alternatives.is_empty() {
        ctx.write_stderr("").ok();
        ctx.write_stderr("You can try one of these commands:").ok();
        for alt_port in alternatives.iter().take(3) {
            ctx.write_stderr(&format!("  serve -p {alt_port}")).ok();
        }
    }

    ExitStatus::ExitedWith(1)
}

/// Handle configuration errors with user-friendly messages
fn handle_config_error(error: &ServeError, ctx: &Context) -> ExitStatus {
    match error {
        ServeError::PortInUse { port } => handle_port_conflict(*port, ctx),
        _ => {
            ctx.write_stderr(&format!("serve: {}", error.user_message()))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

#[cfg(test)]
mod tests;
