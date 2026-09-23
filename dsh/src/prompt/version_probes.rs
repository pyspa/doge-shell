//! Probing the toolchain/cluster/cloud context shown in the prompt (rustc/node/python/go versions, kubectl context, AWS profile, docker context) and the cheap pre-checks that decide whether it's worth
//! spawning each probe at all.
use super::*;

pub async fn fetch_rust_version_async() -> Option<String> {
    use tokio::process::Command;
    let output = Command::new("rustc").arg("--version").output().await.ok()?;

    if output.status.success() {
        // rustc 1.75.0 (82e1608df 2023-12-21)
        let out = String::from_utf8_lossy(&output.stdout);
        let version = out.split_whitespace().nth(1)?;
        Some(version.to_string())
    } else {
        None
    }
}

pub async fn fetch_node_version_async() -> Option<String> {
    use tokio::process::Command;
    let output = Command::new("node").arg("--version").output().await.ok()?;

    if output.status.success() {
        // v20.10.0
        let out = String::from_utf8_lossy(&output.stdout);
        Some(out.trim().to_string())
    } else {
        None
    }
}

pub async fn fetch_python_version_async() -> Option<String> {
    use tokio::process::Command;
    // Try python3 first, then python
    let mut cmd = Command::new("python3");
    cmd.arg("--version");

    let result = cmd.output().await;
    let output = match result {
        Ok(o) => o,
        Err(_) => Command::new("python")
            .arg("--version")
            .output()
            .await
            .ok()?,
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

pub async fn fetch_go_version_async() -> Option<String> {
    use tokio::process::Command;
    let output = Command::new("go").arg("version").output().await.ok()?;

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

pub async fn fetch_k8s_info_async() -> Option<(String, Option<String>)> {
    use tokio::process::Command;
    let output = Command::new("kubectl")
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
    environment: &PromptEnvironment,
) -> Option<String> {
    use tokio::process::Command;
    if let Some(ctx) = trimmed_nonempty(environment.docker_context.as_deref()) {
        return Some(ctx);
    }

    let output = Command::new("docker")
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

/// The slice of shell runtime state the prompt probes read.
///
/// Snapshotted from `Environment` once per refresh tick
/// ([`PromptEnvironment::from_environment`]) so every probe in the tick sees
/// the same values - and so a shell-level unset stays unset. Nothing here is
/// ever re-read from the process environment afterwards: a stale
/// process-global value must not resurrect a runtime shell variable.
///
/// `home` is the `$HOME` shell variable. When the shell has none, the OS
/// account lookup (`dirs::home_dir()`) still backs the `~/.kube/config`
/// fallback: that is a launch-time fact about the user, not a shell
/// variable.
#[derive(Debug, Clone, Default)]
pub(crate) struct PromptEnvironment {
    pub aws_profile: Option<String>,
    pub aws_default_profile: Option<String>,
    pub docker_context: Option<String>,
    pub kubeconfig: Option<String>,
    pub home: Option<String>,
}

impl PromptEnvironment {
    pub(crate) fn from_environment(environment: &Environment) -> Self {
        Self {
            aws_profile: environment.get_var("AWS_PROFILE"),
            aws_default_profile: environment.get_var("AWS_DEFAULT_PROFILE"),
            docker_context: environment.get_var("DOCKER_CONTEXT"),
            kubeconfig: environment.get_var("KUBECONFIG"),
            home: environment.get_var("HOME"),
        }
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

pub(super) fn should_attempt_k8s_context_check_with(environment: &PromptEnvironment) -> bool {
    command_available_cached("kubectl", &KUBECTL_AVAILABLE) && kube_config_present_with(environment)
}

pub(super) fn should_attempt_docker_context_check_from(environment: &PromptEnvironment) -> bool {
    trimmed_nonempty(environment.docker_context.as_deref()).is_some()
        || command_available_cached("docker", &DOCKER_AVAILABLE)
}

fn command_available_cached(command: &'static str, cache: &'static OnceLock<bool>) -> bool {
    *cache.get_or_init(|| which::which(command).is_ok())
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

/// A trimmed, non-blank shell value. Blank counts as unset so an emptied
/// variable falls through to the next source instead of sticking.
fn trimmed_nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
