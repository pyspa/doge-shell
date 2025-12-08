use tracing::debug;

/// Display error in a user-friendly format without stack traces.
///
/// When `log_normal` is true, normal exit conditions are logged for diagnostics; when false
/// the function stays silent for those cases to avoid redundant output in interactive flows.
pub fn display_user_error(err: &anyhow::Error, log_normal: bool) {
    let error_msg = err.to_string();

    if error_msg.contains("unknown command:") {
        if let Some(cmd_start) = error_msg.find("unknown command: ") {
            let rest = &error_msg[cmd_start + 17..];
            // Split command name from suggestion (separated by newline)
            let (cmd, suggestion) = if let Some(newline_pos) = rest.find('\n') {
                (&rest[..newline_pos], Some(&rest[newline_pos + 1..]))
            } else {
                (rest, None)
            };
            eprintln!("dsh: {}: command not found", cmd.trim());
            if let Some(suggestion_msg) = suggestion {
                eprint!("{}", suggestion_msg);
            }
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
