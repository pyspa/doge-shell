//! Probing the toolchain/cluster/cloud context shown in the prompt (rustc/node/python/go versions, kubectl context, AWS profile, docker context) and the cheap pre-checks that decide whether it's worth
//! spawning each probe at all.
//!
//! Every probe resolves its executable through [`PromptRuntimeSnapshot`]
//! and spawns with the snapshot's exported child environment: output
//! parsing lives here, runtime authority lives in `super::runtime`.
use super::runtime::{PromptEnvironment, PromptRuntimeSnapshot, trimmed_nonempty};
use super::*;

pub(crate) async fn fetch_rust_version_async(runtime: &PromptRuntimeSnapshot) -> Option<String> {
    let output = runtime
        .command("rustc")?
        .arg("--version")
        .output()
        .await
        .ok()?;

    if output.status.success() {
        // rustc 1.75.0 (82e1608df 2023-12-21)
        let out = String::from_utf8_lossy(&output.stdout);
        let version = out.split_whitespace().nth(1)?;
        Some(version.to_string())
    } else {
        None
    }
}

pub(crate) async fn fetch_node_version_async(runtime: &PromptRuntimeSnapshot) -> Option<String> {
    let output = runtime
        .command("node")?
        .arg("--version")
        .output()
        .await
        .ok()?;

    if output.status.success() {
        // v20.10.0
        let out = String::from_utf8_lossy(&output.stdout);
        Some(out.trim().to_string())
    } else {
        None
    }
}

pub(crate) async fn fetch_python_version_async(runtime: &PromptRuntimeSnapshot) -> Option<String> {
    // Try python3 first, then python. A missing logical `python3` is the
    // same as a spawn failure: fall through to `python` without ever
    // consulting the process-global PATH.
    let output = if let Some(mut command) = runtime.command("python3") {
        match command.arg("--version").output().await {
            Ok(output) => output,
            Err(_) => runtime
                .command("python")?
                .arg("--version")
                .output()
                .await
                .ok()?,
        }
    } else {
        runtime
            .command("python")?
            .arg("--version")
            .output()
            .await
            .ok()?
    };

    if output.status.success() {
        // Python 3.10.12
        let out = String::from_utf8_lossy(&output.stdout);
        let version = out.split_whitespace().nth(1)?;
        Some(version.to_string())
    } else {
        None
    }
}

pub(crate) async fn fetch_go_version_async(runtime: &PromptRuntimeSnapshot) -> Option<String> {
    let output = runtime.command("go")?.arg("version").output().await.ok()?;

    if output.status.success() {
        // go version go1.21.5 linux/amd64
        let out = String::from_utf8_lossy(&output.stdout);
        let version_tag = out.split_whitespace().nth(2)?; // go1.21.5
        Some(
            version_tag
                .strip_prefix("go")
                .unwrap_or(version_tag)
                .to_string(),
        )
    } else {
        None
    }
}

pub(crate) async fn fetch_k8s_info_async(
    runtime: &PromptRuntimeSnapshot,
) -> Option<(String, Option<String>)> {
    let output = runtime
        .command("kubectl")?
        .arg("config")
        .arg("view")
        .arg("--minify")
        .arg("--output")
        .arg("jsonpath={.current-context}|{.contexts[0].context.namespace}")
        .output()
        .await
        .ok()?;

    if output.status.success() {
        let out = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<&str> = out.split('|').collect();
        let context = parts.first().map(|s| s.to_string())?;
        let namespace = parts
            .get(1)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        Some((context, namespace))
    } else {
        None
    }
}

pub(crate) fn fetch_aws_profile_from(environment: &PromptEnvironment) -> Option<String> {
    resolve_aws_profile(
        environment.aws_profile.as_deref(),
        environment.aws_default_profile.as_deref(),
    )
}

pub(crate) async fn fetch_docker_context_async_from(
    runtime: &PromptRuntimeSnapshot,
) -> Option<String> {
    if let Some(ctx) = trimmed_nonempty(runtime.environment.docker_context.as_deref()) {
        return Some(ctx);
    }

    let output = runtime
        .command("docker")?
        .arg("context")
        .arg("show")
        .output()
        .await
        .ok()?;

    if output.status.success() {
        let out = String::from_utf8_lossy(&output.stdout);
        Some(out.trim().to_string())
    } else {
        None
    }
}

/// Pure `AWS_PROFILE` / `AWS_DEFAULT_PROFILE` precedence: the profile wins,
/// the default profile is the fallback, and a blank value counts as unset.
pub(crate) fn resolve_aws_profile(
    aws_profile: Option<&str>,
    aws_default_profile: Option<&str>,
) -> Option<String> {
    trimmed_nonempty(aws_profile).or_else(|| trimmed_nonempty(aws_default_profile))
}

pub(super) fn should_attempt_k8s_context_check_with(runtime: &PromptRuntimeSnapshot) -> bool {
    runtime.resolve_program("kubectl").is_some() && kube_config_present_with(&runtime.environment)
}

pub(super) fn should_attempt_docker_context_check_from(runtime: &PromptRuntimeSnapshot) -> bool {
    trimmed_nonempty(runtime.environment.docker_context.as_deref()).is_some()
        || runtime.resolve_program("docker").is_some()
}

pub(super) fn kube_config_present_with(environment: &PromptEnvironment) -> bool {
    let kubeconfig = environment.kubeconfig.as_deref().map(OsStr::new);
    match environment.home.as_deref().map(PathBuf::from) {
        Some(home) => kube_config_present_from(kubeconfig, Some(&home)),
        // No `$HOME` in the shell: the OS account home is a launch-time
        // fact, not a shell variable, so it still backs the fallback.
        None => kube_config_present_from(kubeconfig, dirs::home_dir().as_deref()),
    }
}

pub(super) fn kube_config_present_from(
    kubeconfig: Option<&OsStr>,
    home_dir: Option<&Path>,
) -> bool {
    if let Some(value) = kubeconfig
        && !value.to_string_lossy().trim().is_empty()
    {
        return std::env::split_paths(value).any(|path| path.exists());
    }

    home_dir.is_some_and(|home| home.join(".kube").join("config").exists())
}
