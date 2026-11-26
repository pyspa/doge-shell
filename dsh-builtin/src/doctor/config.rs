//! `doctor config`: existing file diagnostics and finite effective runtime values.
use crate::effective_config::{ConfigLoadReport, ConfigLoadStatus};
use crate::shell_capabilities::EffectiveConfigCapability;
use dsh_types::Context;
use serde_json::{Value, json};
use std::fs;

pub(super) fn check_config(ctx: &Context, proxy: &dyn EffectiveConfigCapability) {
    let (config_path, load_status) = config_load(proxy);
    let label = match load_status {
        Some(ConfigLoadStatus::Loaded) => "ok config loaded",
        Some(ConfigLoadStatus::Missing) => "warn config missing",
        Some(ConfigLoadStatus::ReadFailed) => "warn config read-failed",
        Some(ConfigLoadStatus::EvaluationFailed) => {
            "warn config evaluation-failed (previous preferences retained)"
        }
        None => "skip config load status unavailable",
    };
    let _ = ctx.write_stdout(&format!("{label} {}", config_path.display()));

    let skills_dir = crate::config_paths::skills_dir();
    if skills_dir.exists() {
        let count = fs::read_dir(&skills_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);
        let _ = ctx.write_stdout(&format!(
            "ok runtime-skills {} entries={count}",
            skills_dir.display()
        ));
    } else {
        let _ = ctx.write_stdout(&format!("warn missing {}", skills_dir.display()));
    }

    match effective_values(proxy) {
        Value::Object(values) => {
            let _ =
                ctx.write_stdout("ok effective runtime config (auto_notify_threshold in seconds)");
            for (key, value) in values {
                let label = value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string());
                let _ = ctx.write_stdout(&format!("  {key}={label}"));
            }
        }
        _ => {
            let _ = ctx.write_stdout("warn runtime-config-unavailable");
        }
    }
}

fn effective_values(proxy: &dyn EffectiveConfigCapability) -> Value {
    json!(proxy.effective_config())
}

pub(super) fn config_details(proxy: &dyn EffectiveConfigCapability) -> Value {
    let (config, load_status) = config_load(proxy);
    let skills = crate::config_paths::skills_dir();
    let effective = effective_values(proxy);
    let mut details = json!({
        "config": {"path": config, "exists": config.is_file(), "load_status": load_status},
        "runtime_skills": {
            "path": skills, "exists": skills.is_dir(),
            "entries": fs::read_dir(&skills).map(|entries| entries.count()).unwrap_or(0)
        },
        "effective": effective
    });
    if effective.is_null() {
        details["effective_error"] = json!("runtime-config-unavailable");
    }
    details
}

fn config_load(
    proxy: &dyn EffectiveConfigCapability,
) -> (std::path::PathBuf, Option<ConfigLoadStatus>) {
    match proxy.config_load_report() {
        Some(ConfigLoadReport { path, status }) => (path, Some(status)),
        None => (crate::config_paths::config_home().join("config.lisp"), None),
    }
}
