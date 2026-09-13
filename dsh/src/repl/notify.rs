//! Desktop notifications for long-running commands and background jobs.

use crate::suggestion::InputPreferences;
use std::time::Duration;
use tracing::warn;

/// Longest command text embedded in a notification body.
const CMD_PREVIEW_CHARS: usize = 50;

/// Send a desktop notification if `prefs` enable it and the command ran longer
/// than the configured threshold.
///
/// Shared by the foreground execution path and the background job reaper so the
/// two cannot drift apart.
pub(crate) fn notify_command_finished(
    prefs: &InputPreferences,
    cmd: &str,
    elapsed: Duration,
    exit_code: i32,
) {
    if !prefs.auto_notify_enabled {
        return;
    }
    if elapsed < Duration::from_secs(prefs.auto_notify_threshold) {
        return;
    }

    let summary = if exit_code == 0 {
        "Command Completed"
    } else {
        "Command Failed"
    };
    let body = format!(
        "'{}' took {:.1}s",
        preview_command(cmd),
        elapsed.as_secs_f64()
    );

    // Fire and forget.
    if let Err(e) = notify_rust::Notification::new()
        .summary(summary)
        .body(&body)
        .appname("doge-shell")
        .show()
    {
        warn!("Failed to send desktop notification: {}", e);
    }
}

/// Truncate on a character boundary — byte slicing here panics on multi-byte
/// input such as a Japanese commit message.
fn preview_command(cmd: &str) -> String {
    if cmd.chars().count() <= CMD_PREVIEW_CHARS {
        return cmd.to_string();
    }
    let truncated: String = cmd.chars().take(CMD_PREVIEW_CHARS - 3).collect();
    format!("{}...", truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_command_keeps_short_commands_intact() {
        assert_eq!(preview_command("cargo test"), "cargo test");
    }

    #[test]
    fn preview_command_truncates_long_commands() {
        let long = "a".repeat(120);
        let out = preview_command(&long);
        assert_eq!(out.chars().count(), CMD_PREVIEW_CHARS);
        assert!(out.ends_with("..."));
    }

    #[test]
    fn preview_command_does_not_split_multibyte_chars() {
        // Byte slicing at 47 would land mid-character and panic.
        let long = "あ".repeat(80);
        let out = preview_command(&long);
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().count(), CMD_PREVIEW_CHARS);
    }

    #[test]
    fn notify_is_skipped_when_disabled() {
        let prefs = InputPreferences {
            auto_notify_enabled: false,
            ..Default::default()
        };
        // Must not panic and must not attempt to reach the notification daemon.
        notify_command_finished(&prefs, "sleep 60", Duration::from_secs(60), 0);
    }

    #[test]
    fn notify_is_skipped_below_threshold() {
        let prefs = InputPreferences {
            auto_notify_enabled: true,
            auto_notify_threshold: 10,
            ..Default::default()
        };
        notify_command_finished(&prefs, "sleep 1", Duration::from_secs(1), 0);
    }
}
