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

pub fn fetch_aws_profile() -> Option<String> {
    std::env::var("AWS_PROFILE")
        .ok()
        .or_else(|| std::env::var("AWS_DEFAULT_PROFILE").ok())
}

pub async fn fetch_docker_context_async() -> Option<String> {
    use tokio::process::Command;
    if let Some(ctx) = env_var_value("DOCKER_CONTEXT") {
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

pub(super) fn should_attempt_k8s_context_check() -> bool {
    command_available_cached("kubectl", &KUBECTL_AVAILABLE) && kube_config_present()
}

pub(super) fn should_attempt_docker_context_check() -> bool {
    env_var_present("DOCKER_CONTEXT") || command_available_cached("docker", &DOCKER_AVAILABLE)
}

fn command_available_cached(command: &'static str, cache: &'static OnceLock<bool>) -> bool {
    *cache.get_or_init(|| which::which(command).is_ok())
}

fn kube_config_present() -> bool {
    kube_config_present_from(
        std::env::var_os("KUBECONFIG").as_deref(),
        dirs::home_dir().as_deref(),
    )
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

fn env_var_present(name: &str) -> bool {
    env_var_value(name).is_some()
}

fn env_var_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
