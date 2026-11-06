use dsh_types::{
    Context,
    observed_output::{ObservedOutput, SharedOutputObserver},
};
use std::os::fd::IntoRawFd;
fn observed_context() -> (Context, SharedOutputObserver) {
    let mut ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), false);
    let observer = ObservedOutput::shared(8192);
    ctx.output_observer = Some(observer.clone());
    ctx.outfile = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
    (ctx, observer)
}
fn observed_stdout(observer: &SharedOutputObserver) -> String {
    observer.lock().unwrap().snapshot().stdout
}
use crate::effective_config::{CommandLedgerMode, EffectiveConfig, SuggestionMode};
use crate::test_support::TestShellProxy;

fn sample() -> EffectiveConfig {
    EffectiveConfig {
        suggestion_mode: SuggestionMode::Off,
        transient_prompt: false,
        auto_pair: true,
        status_line: true,
        failure_hint: false,
        auto_notify_enabled: true,
        auto_notify_threshold: 37,
        command_ledger_mode: CommandLedgerMode::Output,
        pipefail: true,
    }
}

#[test]
fn effective_config_has_only_fixed_typed_keys_and_shared_text_projection() {
    let proxy = TestShellProxy {
        effective_config: Some(sample()),
        ..Default::default()
    };
    let details = super::config::config_details(&proxy);
    assert!(details["config"]["path"].is_string());
    assert!(details["config"]["exists"].is_boolean());
    assert!(details["runtime_skills"]["entries"].is_number());
    let effective = details["effective"].as_object().unwrap();
    assert_eq!(
        effective.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "auto_notify_enabled",
            "auto_notify_threshold",
            "auto_pair",
            "command_ledger_mode",
            "failure_hint",
            "pipefail",
            "status_line",
            "suggestion_mode",
            "transient_prompt"
        ]
    );
    assert_eq!(effective["suggestion_mode"], "off");
    assert_eq!(effective["command_ledger_mode"], "output");
    assert_eq!(effective["auto_notify_threshold"], 37);
    let (ctx, observer) = observed_context();
    super::config::check_config(&ctx, &proxy);
    let text = observed_stdout(&observer);
    assert!(text.contains("runtime-skills") || text.contains("skills"));
    for (key, value) in effective {
        let label = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        assert!(text.contains(&format!("{key}={label}")), "{key}: {text}");
        if !matches!(
            key.as_str(),
            "suggestion_mode" | "command_ledger_mode" | "auto_notify_threshold"
        ) {
            assert!(value.is_boolean());
        }
    }
    assert!(proxy.vars.is_empty());
    assert!(proxy.dispatched.is_empty());
}

#[test]
fn effective_config_unavailable_is_explicit_and_summary_matches_config_section() {
    let mut proxy = TestShellProxy::default();
    let details = super::config::config_details(&proxy);
    assert!(details["effective"].is_null());
    assert_eq!(details["effective_error"], "runtime-config-unavailable");
    let (ctx, observer) = observed_context();
    super::config::check_config(&ctx, &proxy);
    assert!(observed_stdout(&observer).contains("runtime-config-unavailable"));
    proxy.effective_config = Some(sample());
    let options = super::DoctorOptions {
        section: Some("config".to_string()),
        json: true,
        prompt_size: false,
        section_args: Vec::new(),
    };
    let section =
        super::json::json_section_details(&mut proxy, std::path::Path::new("/"), &options);
    let all = super::DoctorOptions {
        section: None,
        ..options
    };
    let summary = super::json::json_section_details(&mut proxy, std::path::Path::new("/"), &all);
    assert_eq!(summary["config"], section);
}

#[test]
fn effective_config_helpers_require_only_one_finite_snapshot_read() {
    use crate::shell_capabilities::EffectiveConfigCapability;
    struct SnapshotOnly(std::cell::Cell<usize>);
    impl EffectiveConfigCapability for SnapshotOnly {
        fn effective_config(&self) -> Option<EffectiveConfig> {
            self.0.set(self.0.get() + 1);
            Some(sample())
        }
    }
    // This host has no variable/config-key access, dispatch, or probe API.
    let host = SnapshotOnly(std::cell::Cell::new(0));
    let _ = super::config::config_details(&host);
    assert_eq!(host.0.get(), 1);
    let (ctx, _) = observed_context();
    super::config::check_config(&ctx, &host);
    assert_eq!(host.0.get(), 2);
}
