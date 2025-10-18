use tracing::debug;

/// Display error in a user-friendly format without stack traces.
///
/// When `log_normal` is true, normal exit conditions are logged for diagnostics; when false
/// the function stays silent for those cases to avoid redundant output in interactive flows.
pub fn display_user_error(err: &anyhow::Error, log_normal: bool) {
    let error_msg = err.to_string();

    if error_msg.contains("unknown command:") {
        if let Some(cmd_start) = error_msg.find("unknown command: ") {
            let cmd = &error_msg[cmd_start + 17..];
            eprintln!("dsh: {}: command not found", cmd.trim());
        } else {
            eprintln!("dsh: command not found");
        }
    } else if error_msg.contains("Shell terminated by double Ctrl+C")
        || error_msg.contains("Normal exit")
        || error_msg.contains("Exit by")
    {
        if log_normal {
            debug!("Shell exiting normally: {}", error_msg);
        }
    } else {
        eprintln!("dsh: {}", error_msg);
    }
}
