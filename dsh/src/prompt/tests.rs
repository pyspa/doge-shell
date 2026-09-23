use super::Prompt;
use super::context::PromptContext;
use super::modules::PromptModule;
use super::modules::execution_time::ExecutionTimeModule;
use super::modules::exit_status::ExitStatusModule;
use super::modules::nodejs::NodeModule;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn test_execution_time_module_short() {
    let module = ExecutionTimeModule::new();
    let current_dir = PathBuf::from("/");
    let context = PromptContext {
        current_dir: &current_dir,
        project_root: None,
        git_root: None,
        git_status: None,
        has_rust_project: false,
        has_node_project: false,
        has_python_project: false,
        has_go_project: false,
        rust_version: None,
        rust_source: None,
        node_version: None,
        node_source: None,
        python_version: None,
        python_source: None,
        go_version: None,
        go_source: None,
        k8s_context: None,
        k8s_namespace: None,
        aws_profile: None,
        docker_context: None,
        last_exit_status: 0,
        last_duration: Some(Duration::from_secs(5)),
    };

    let output = module.render(&context).unwrap();
    assert!(output.contains("5s"));
}

#[test]
fn test_execution_time_module_long() {
    let module = ExecutionTimeModule::new();
    let current_dir = PathBuf::from("/");
    let context = PromptContext {
        current_dir: &current_dir,
        project_root: None,
        git_root: None,
        git_status: None,
        has_rust_project: false,
        has_node_project: false,
        has_python_project: false,
        has_go_project: false,
        rust_version: None,
        rust_source: None,
        node_version: None,
        node_source: None,
        python_version: None,
        python_source: None,
        go_version: None,
        go_source: None,
        k8s_context: None,
        k8s_namespace: None,
        aws_profile: None,
        docker_context: None,
        last_exit_status: 0,
        last_duration: Some(Duration::from_secs(65)),
    };

    let output = module.render(&context).unwrap();
    assert!(output.contains("1m5s"));
}

#[test]
fn test_execution_time_module_none_under_threshold() {
    let module = ExecutionTimeModule::new();
    let current_dir = PathBuf::from("/");
    let context = PromptContext {
        current_dir: &current_dir,
        project_root: None,
        git_root: None,
        git_status: None,
        has_rust_project: false,
        has_node_project: false,
        has_python_project: false,
        has_go_project: false,
        rust_version: None,
        rust_source: None,
        node_version: None,
        node_source: None,
        python_version: None,
        python_source: None,
        go_version: None,
        go_source: None,
        k8s_context: None,
        k8s_namespace: None,
        aws_profile: None,
        docker_context: None,
        last_exit_status: 0,
        last_duration: Some(Duration::from_secs(1)),
    };

    assert!(module.render(&context).is_none());
}

#[test]
fn test_exit_status_module_success() {
    let module = ExitStatusModule::new();
    let current_dir = PathBuf::from("/");
    let context = PromptContext {
        current_dir: &current_dir,
        project_root: None,
        git_root: None,
        git_status: None,
        has_rust_project: false,
        has_node_project: false,
        has_python_project: false,
        has_go_project: false,
        rust_version: None,
        rust_source: None,
        node_version: None,
        node_source: None,
        python_version: None,
        python_source: None,
        go_version: None,
        go_source: None,
        k8s_context: None,
        k8s_namespace: None,
        aws_profile: None,
        docker_context: None,
        last_exit_status: 0,
        last_duration: None,
    };

    assert!(module.render(&context).is_none());
}

#[test]
fn test_exit_status_module_failure() {
    let module = ExitStatusModule::new();
    let current_dir = PathBuf::from("/");
    let context = PromptContext {
        current_dir: &current_dir,
        project_root: None,
        git_root: None,
        git_status: None,
        has_rust_project: false,
        has_node_project: false,
        has_python_project: false,
        has_go_project: false,
        rust_version: None,
        rust_source: None,
        node_version: None,
        node_source: None,
        python_version: None,
        python_source: None,
        go_version: None,
        go_source: None,
        k8s_context: None,
        k8s_namespace: None,
        aws_profile: None,
        docker_context: None,
        last_exit_status: 127,
        last_duration: None,
    };

    let output = module.render(&context).unwrap();
    assert!(output.contains("✘"));
    assert!(output.contains("127"));
}

#[test]
fn test_parse_git_status_output() {
    let output = r#"# branch.oid (hash)
# branch.head master
# branch.ab +1 -1
1 .M N... 100644 100644 100644 (hash) (hash) modified_file
1 M. N... 100644 100644 100644 (hash) (hash) staged_file
? untracked_file
u UU N... 100644 100644 100644 (hash) (hash) conflicted_file
2 R. N... 100644 100644 100644 (hash) (hash) R100 renamed_file orig_file
"#;

    // Use the pub(crate) function from parent module
    use super::parse_git_status_output;

    let status = parse_git_status_output(output.as_bytes()).unwrap();

    assert_eq!(status.branch, "master");
    assert_eq!(status.ahead, 1);
    assert_eq!(status.behind, 1);
    assert_eq!(status.modified, 1); // modified_file
    assert_eq!(status.staged, 2); // staged_file + renamed_file
    assert_eq!(status.untracked, 1);
    assert_eq!(status.conflicted, 1);
    assert_eq!(status.renamed, 1);
}

#[test]
fn node_module_uses_project_root_and_runtime_source() {
    let module = NodeModule::new();
    let dir = tempdir().unwrap();
    let project_root = dir.path().join("web");
    let nested = project_root.join("src");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(project_root.join("package.json"), "{\"name\":\"web\"}").unwrap();

    let context = PromptContext {
        current_dir: &nested,
        project_root: Some(&project_root),
        git_root: None,
        git_status: None,
        has_rust_project: false,
        has_node_project: true,
        has_python_project: false,
        has_go_project: false,
        rust_version: None,
        rust_source: None,
        node_version: Some("v20.11.0"),
        node_source: Some(".nvmrc"),
        python_version: None,
        python_source: None,
        go_version: None,
        go_source: None,
        k8s_context: None,
        k8s_namespace: None,
        aws_profile: None,
        docker_context: None,
        last_exit_status: 0,
        last_duration: None,
    };

    let output = module.render(&context).unwrap();
    assert!(output.contains("v20.11.0"));
    assert!(output.contains("(.nvmrc)"));
}

#[test]
fn prompt_detects_project_types_from_parent_directory() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();
    std::fs::write(dir.path().join(".nvmrc"), "20.11.0\n").unwrap();
    let nested = dir.path().join("src").join("nested");
    std::fs::create_dir_all(&nested).unwrap();

    let mut prompt = Prompt::new(nested.clone(), "🐕 < ".to_string());
    prompt.set_current(&nested);

    assert!(prompt.needs_node_check());
}

#[test]
fn kube_config_present_uses_existing_explicit_env() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("kubeconfig");
    std::fs::write(&config, "apiVersion: v1\n").unwrap();

    assert!(super::kube_config_present_from(
        Some(config.as_os_str()),
        None
    ));
}

#[test]
fn kube_config_present_rejects_missing_explicit_env() {
    assert!(!super::kube_config_present_from(
        Some(OsStr::new("/missing/kubeconfig")),
        None
    ));
}

#[test]
fn kube_config_present_uses_any_existing_env_path() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("missing");
    let config = dir.path().join("kubeconfig");
    std::fs::write(&config, "apiVersion: v1\n").unwrap();
    let paths = std::env::join_paths([missing.as_os_str(), config.as_os_str()]).unwrap();

    assert!(super::kube_config_present_from(
        Some(paths.as_os_str()),
        None
    ));
}

#[test]
fn kube_config_present_ignores_empty_env_without_home_config() {
    assert!(!super::kube_config_present_from(
        Some(OsStr::new(" ")),
        None
    ));
}

#[test]
fn kube_config_present_falls_back_to_home_file() {
    let dir = tempdir().unwrap();
    let kube_dir = dir.path().join(".kube");
    std::fs::create_dir_all(&kube_dir).unwrap();
    std::fs::write(kube_dir.join("config"), "apiVersion: v1\n").unwrap();

    assert!(super::kube_config_present_from(None, Some(dir.path())));
}

#[test]
fn kube_config_present_empty_env_falls_back_to_home_file() {
    let dir = tempdir().unwrap();
    let kube_dir = dir.path().join(".kube");
    std::fs::create_dir_all(&kube_dir).unwrap();
    std::fs::write(kube_dir.join("config"), "apiVersion: v1\n").unwrap();

    assert!(super::kube_config_present_from(
        Some(OsStr::new("")),
        Some(dir.path())
    ));
}

use super::version_probes::{PromptEnvironment, fetch_aws_profile_from, resolve_aws_profile};
use crate::ProcessEnvGuard;
use crate::environment::Environment;

/// A shell with none of the prompt-relevant keys set, even if the ambient
/// process environment happens to carry them.
fn shell_only_environment() -> std::sync::Arc<parking_lot::RwLock<Environment>> {
    let environment = Environment::new();
    {
        let mut env = environment.write();
        for key in [
            "AWS_PROFILE",
            "AWS_DEFAULT_PROFILE",
            "DOCKER_CONTEXT",
            "KUBECONFIG",
            "HOME",
        ] {
            env.unset_shell_var(key);
        }
    }
    environment
}

#[test]
fn resolve_aws_profile_prefers_the_profile_over_the_default() {
    assert_eq!(
        resolve_aws_profile(Some("shell-profile"), Some("shell-default")),
        Some("shell-profile".to_string())
    );
    assert_eq!(
        resolve_aws_profile(None, Some("shell-default")),
        Some("shell-default".to_string())
    );
    assert_eq!(resolve_aws_profile(None, None), None);
    assert_eq!(
        resolve_aws_profile(Some("   "), Some("shell-default")),
        Some("shell-default".to_string()),
        "a blank profile counts as unset and falls through"
    );
}

#[test]
fn aws_profile_snapshot_wins_over_the_default_profile() {
    let environment = PromptEnvironment {
        aws_profile: Some("shell-profile".to_string()),
        aws_default_profile: Some("shell-default".to_string()),
        ..Default::default()
    };

    assert_eq!(
        fetch_aws_profile_from(&environment),
        Some("shell-profile".to_string())
    );
}

/// A process-only stale `AWS_PROFILE` never reaches the prompt: the shell
/// snapshot says nothing, so the resolver says nothing.
#[test]
fn process_only_aws_profile_is_ignored() {
    let _lock = crate::test_env_lock();
    let environment = shell_only_environment();
    let _stale = ProcessEnvGuard::set("AWS_PROFILE", "stale-profile");
    let _stale_default = ProcessEnvGuard::set("AWS_DEFAULT_PROFILE", "stale-default");

    let snapshot = PromptEnvironment::from_environment(&environment.read());
    assert_eq!(snapshot.aws_profile, None);
    assert_eq!(snapshot.aws_default_profile, None);
    assert_eq!(fetch_aws_profile_from(&snapshot), None);
}

/// A shell `DOCKER_CONTEXT` is used as-is, without spawning `docker`.
#[test]
fn shell_docker_context_is_used_without_a_subprocess() {
    let _lock = crate::test_env_lock();
    let environment = shell_only_environment();
    environment
        .write()
        .set_shell_var("DOCKER_CONTEXT".to_string(), "shell-context".to_string());
    let _stale = ProcessEnvGuard::set("DOCKER_CONTEXT", "stale-context");

    let snapshot = PromptEnvironment::from_environment(&environment.read());
    assert_eq!(snapshot.docker_context.as_deref(), Some("shell-context"));
}

/// A shell-level unset of `DOCKER_CONTEXT` does not resurrect the stale
/// process value in the snapshot.
#[test]
fn unset_docker_context_does_not_resurrect_the_process_value() {
    let _lock = crate::test_env_lock();
    let environment = shell_only_environment();
    let _stale = ProcessEnvGuard::set("DOCKER_CONTEXT", "stale-context");

    let snapshot = PromptEnvironment::from_environment(&environment.read());
    assert_eq!(snapshot.docker_context, None);
}

/// A shell `DOCKER_CONTEXT` enables the docker gate without needing the
/// `docker` binary to be present.
#[test]
fn shell_docker_context_enables_the_docker_gate() {
    use super::version_probes::should_attempt_docker_context_check_from;
    let environment = PromptEnvironment {
        docker_context: Some("shell-context".to_string()),
        ..Default::default()
    };

    assert!(should_attempt_docker_context_check_from(&environment));
}

/// The snapshot's shell `HOME` backs the `~/.kube/config` fallback, and an
/// explicit shell `KUBECONFIG` wins over it.
#[test]
fn kube_snapshot_uses_shell_home_and_kubeconfig() {
    use super::version_probes::kube_config_present_with;
    let dir = tempdir().unwrap();
    let kube_dir = dir.path().join(".kube");
    std::fs::create_dir_all(&kube_dir).unwrap();
    std::fs::write(kube_dir.join("config"), "apiVersion: v1\n").unwrap();

    let with_home = PromptEnvironment {
        home: Some(dir.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert!(kube_config_present_with(&with_home));

    let empty = tempdir().unwrap();
    let without_home_config = PromptEnvironment {
        home: Some(empty.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert!(!kube_config_present_with(&without_home_config));

    let config = dir.path().join("kubeconfig");
    std::fs::write(&config, "apiVersion: v1\n").unwrap();
    let with_explicit = PromptEnvironment {
        kubeconfig: Some(config.to_string_lossy().into_owned()),
        home: Some(empty.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert!(kube_config_present_with(&with_explicit));
}

#[test]
fn kubeconfig_helper_consumes_the_supplied_shell_value() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("kubeconfig");
    std::fs::write(&config, "apiVersion: v1\n").unwrap();

    assert!(super::kube_config_present_from(
        Some(config.as_os_str()),
        None
    ));
    assert!(!super::kube_config_present_from(
        Some(std::ffi::OsStr::new("/missing/kubeconfig")),
        None
    ));
}
