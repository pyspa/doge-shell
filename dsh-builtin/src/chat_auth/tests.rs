use super::*;
use crate::test_support::TestShellProxy;

#[test]
fn status_recovers_from_invalid_transport_settings() {
    let dir = tempfile::tempdir().unwrap();
    let mut proxy = TestShellProxy::default();
    proxy.vars.insert(
        "XDG_CONFIG_HOME".into(),
        std::fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .into(),
    );
    proxy.vars.insert(
        dsh_openai::PROVIDER_ENV.into(),
        "chatgpt_subscription".into(),
    );
    proxy.vars.insert(
        "AI_CHAT_BASE_URL".into(),
        "https://custom.example/v1".into(),
    );
    assert!(load_openai_config(&mut proxy).validate().is_err());
    let pid = nix::unistd::getpid();
    let ctx = Context::new_safe(pid, pid, false);
    execute(&ctx, &["chat_auth".into(), "status".into()], &mut proxy).unwrap();
    assert!(load_openai_config(&mut proxy).validate().is_err());
}

#[test]
fn login_flags_and_all_other_arguments_are_validated_before_store_access() {
    for args in [
        vec!["chat_auth", "login", "--new", "--no-browser"],
        vec!["chat_auth", "login", "--no-browser", "--new"],
    ] {
        let args: Vec<_> = args.into_iter().map(String::from).collect();
        assert_eq!(
            validate_args(&args).unwrap().unwrap(),
            LoginOptions {
                new_account: true,
                no_browser: true
            }
        );
    }
    let pid = nix::unistd::getpid();
    let ctx = Context::new_safe(pid, pid, false);
    for args in [
        vec!["chat_auth", "login", "--no-browser", "--no-browser"],
        vec!["chat_auth", "login", "--unknown"],
        vec!["chat_auth", "logout", "extra"],
        vec!["chat_auth", "status", "extra"],
        vec!["chat_auth", "account", "a", "b"],
    ] {
        let mut proxy = TestShellProxy::default();
        let args: Vec<_> = args.into_iter().map(String::from).collect();
        let error = execute(&ctx, &args, &mut proxy).unwrap_err().to_string();
        assert!(error.contains("Usage:"), "{error}");
        assert!(proxy.vars.is_empty());
    }
}

#[test]
fn opener_missing_failure_timeout_and_manual_mode_allow_fallback() {
    use std::os::unix::fs::PermissionsExt;
    let mut proxy = TestShellProxy::default();
    let url = "https://auth.openai.com/api/accounts/authorize?state=mock";
    assert!(
        manual_browser_reason(&mut proxy, url, false, Duration::from_secs(1))
            .unwrap()
            .is_some()
    );
    let dir = tempfile::tempdir().unwrap();
    #[cfg(target_os = "linux")]
    let name = "xdg-open";
    #[cfg(target_os = "macos")]
    let name = "open";
    let program = dir.path().join(name);
    proxy.command_search_paths = vec![dir.path().to_path_buf()];
    let write = |body: &str| {
        std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    };
    write("exit 0");
    assert!(
        manual_browser_reason(&mut proxy, url, false, Duration::from_secs(1))
            .unwrap()
            .is_none()
    );
    write("exit 1");
    assert!(
        manual_browser_reason(&mut proxy, url, false, Duration::from_secs(1))
            .unwrap()
            .is_some()
    );
    write("exec sleep 5");
    let start = Instant::now();
    assert!(
        manual_browser_reason(&mut proxy, url, false, Duration::from_millis(10))
            .unwrap()
            .is_some()
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(
        manual_browser_reason(&mut proxy, url, true, Duration::from_secs(5)).unwrap(),
        Some("Manual browser sign-in requested.")
    );
}

#[test]
fn models_requires_explicit_provider_without_switching_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut proxy = TestShellProxy::default();
    proxy.vars.insert(
        "XDG_CONFIG_HOME".into(),
        std::fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .into(),
    );
    let pid = nix::unistd::getpid();
    let ctx = Context::new_safe(pid, pid, false);
    let error = execute(&ctx, &["chat_auth".into(), "models".into()], &mut proxy)
        .unwrap_err()
        .to_string();
    assert!(error.contains("chat_provider chatgpt_subscription"));
    assert!(!proxy.vars.contains_key(dsh_openai::PROVIDER_ENV));
}

#[test]
fn cancelling_a_waiting_opener_reaps_it_without_manual_fallback() {
    use std::{cell::Cell, os::unix::fs::PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("mock-open");
    std::fs::write(&program, "#!/bin/sh\nexec sleep 5\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut proxy = TestShellProxy {
        command_search_paths: vec![dir.path().into()],
        ..Default::default()
    };
    let mut child = crate::runtime_spawn::runtime_command(&mut proxy, "mock-open")
        .unwrap()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let checks = Cell::new(0);
    let start = Instant::now();
    let result = wait_for_browser(&mut child, Duration::from_secs(5), &|| {
        checks.set(checks.get() + 1);
        checks.get() >= 2
    });
    assert!(browser_result(result).unwrap_err().is::<LoginCancelled>());
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(child.try_wait().unwrap().is_some());
}
