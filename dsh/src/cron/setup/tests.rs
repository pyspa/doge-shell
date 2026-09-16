use super::*;

#[test]
fn the_crontab_line_runs_cron_tick_every_minute() {
    let line = crontab_line();
    assert!(line.starts_with("* * * * *"), "{line}");
    assert!(line.contains("cron tick"), "{line}");
    // Silences a `crontab -e` prompt or mail on every run - the point of the
    // suppression is documented in `tick.rs`, this just checks it is here.
    assert!(line.contains(">/dev/null 2>&1"), "{line}");
}

#[test]
fn the_systemd_unit_has_both_files_and_an_enable_command() {
    let text = systemd_unit();
    assert!(text.contains("dogesh-cron.service"));
    assert!(text.contains("dogesh-cron.timer"));
    assert!(text.contains("OnCalendar=*-*-* *:*:00"));
    assert!(text.contains("systemctl --user enable --now dogesh-cron.timer"));
    assert!(text.contains("enable-linger"));
    assert!(text.contains("cron tick"));
}

#[test]
fn the_launchd_plist_is_well_formed_and_has_a_bootstrap_command() {
    let text = launchd_plist();
    assert!(text.contains("<?xml"));
    assert!(text.contains("<key>Label</key>"));
    assert!(text.contains("StartInterval"));
    assert!(text.contains("cron tick"));
    assert!(text.contains("launchctl bootstrap gui/$(id -u)"));
    assert!(text.contains("launchctl print gui/$(id -u)"));
}

/// The whole reason this prints text instead of shelling out: every format
/// must be produceable - and testable - on any host, Linux included.
#[test]
fn every_format_is_generated_regardless_of_the_test_host() {
    assert!(!crontab_line().is_empty());
    assert!(!systemd_unit().is_empty());
    assert!(!launchd_plist().is_empty());
}
