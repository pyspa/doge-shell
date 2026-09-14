//! `cron setup` — prints the external-tick unit for a system, never installs
//! it.
//!
//! This never calls `systemctl`, `launchctl` or edits a crontab. Text goes to
//! stdout for a person to read and paste; a doge-shell has no business
//! rewriting a system service file on its own say-so. Printing plain text
//! also means the systemd and launchd output can be generated - and
//! tested - identically on any host: no `#[cfg(target_os = ...)]` arm is
//! needed here, so nothing about this file requires the macOS CI job to
//! prove it works.

use anyhow::Result;
use dsh_types::Context;

fn current_exe() -> String {
    std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "dsh".to_string())
}

pub fn crontab_line() -> String {
    format!(
        "* * * * * {} -c \"cron tick\" >/dev/null 2>&1",
        current_exe()
    )
}

pub fn systemd_unit() -> String {
    let exe = current_exe();
    format!(
        "# ~/.config/systemd/user/dsh-cron.service\n\
         [Unit]\n\
         Description=doge-shell cron tick\n\n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={exe} -c \"cron tick\"\n\n\
         # ~/.config/systemd/user/dsh-cron.timer\n\
         [Unit]\n\
         Description=Run doge-shell cron tick every minute\n\n\
         [Timer]\n\
         OnCalendar=*-*-* *:*:00\n\
         Persistent=true\n\n\
         [Install]\n\
         WantedBy=timers.target\n\n\
         # Then, once both files are in place:\n\
         #   systemctl --user enable --now dsh-cron.timer\n\
         #   loginctl enable-linger \"$USER\"   # so it runs without a login session\n\
         # Verify it is firing:\n\
         #   systemctl --user list-timers dsh-cron.timer\n"
    )
}

pub fn launchd_plist() -> String {
    let exe = current_exe();
    format!(
        "<!-- ~/Library/LaunchAgents/dev.doge-shell.cron.plist -->\n\
         <?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
           \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>dev.doge-shell.cron</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{exe}</string>\n\
         \t\t<string>-c</string>\n\
         \t\t<string>cron tick</string>\n\
         \t</array>\n\
         \t<key>StartInterval</key>\n\
         \t<integer>60</integer>\n\
         \t<key>RunAtLoad</key>\n\
         \t<false/>\n\
         </dict>\n\
         </plist>\n\n\
         <!-- Then: -->\n\
         <!--   launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.doge-shell.cron.plist -->\n\
         <!-- Verify it is loaded: -->\n\
         <!--   launchctl print gui/$(id -u)/dev.doge-shell.cron -->\n"
    )
}

pub fn command(ctx: &Context, args: &[String]) -> Result<()> {
    let text = if args.iter().any(|a| a == "--systemd") {
        systemd_unit()
    } else if args.iter().any(|a| a == "--launchd") {
        launchd_plist()
    } else if args.iter().any(|a| a == "--crontab") {
        crontab_line()
    } else if cfg!(target_os = "macos") {
        launchd_plist()
    } else {
        format!(
            "# crontab -e, then add:\n{}\n\n# Or, for a systemd user timer instead:\n{}",
            crontab_line(),
            systemd_unit()
        )
    };
    // The branches disagree among themselves on a trailing newline (some end
    // in one, `crontab_line` does not); trim it here rather than fix each,
    // so `write_stdout`'s own `writeln!` is the only thing that adds one.
    ctx.write_stdout(text.trim_end_matches('\n'))?;
    Ok(())
}

#[cfg(test)]
mod tests;
