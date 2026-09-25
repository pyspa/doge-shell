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

use super::runtime::PromptEnvironment;
use super::runtime::PromptRuntimeSnapshot;
use super::version_probes::{fetch_aws_profile_from, resolve_aws_profile};
use super::{fetch_node_version_async, fetch_python_version_async};
use crate::ProcessEnvGuard;
use crate::environment::Environment;
use parking_lot::RwLock;
use std::sync::Arc;

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
    use super::runtime::PromptRuntimeSnapshot;
    use super::version_probes::should_attempt_docker_context_check_from;
    let _lock = crate::test_env_lock();
    let environment = shell_only_environment();
    environment
        .write()
        .set_shell_var("DOCKER_CONTEXT".to_string(), "shell-context".to_string());

    let runtime = PromptRuntimeSnapshot::from_environment(&environment.read(), PathBuf::from("/"));
    assert!(should_attempt_docker_context_check_from(&runtime));
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

/// Write a Unix executable script for probe tests.
fn write_executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

/// Drive a probe future inside a sync test so the process-environment lock
/// stays held for the whole probe (`clippy::await_holding_lock` forbids
/// holding the sync guard across `.await`).
fn block_on_probe<Fut>(future: Fut) -> Fut::Output
where
    Fut: std::future::Future,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("prompt probe test runtime")
        .block_on(future)
}

/// A shell with none of the prompt-relevant keys set plus a logical PATH.
/// The process environment may still hold stale values; the snapshot must
/// never see them.
fn prompt_test_environment(logical_path: &str) -> Arc<RwLock<Environment>> {
    let environment = shell_only_environment();
    environment
        .write()
        .set_shell_var("PATH".to_string(), logical_path.to_string());
    environment
}

fn runtime_for(environment: &Arc<RwLock<Environment>>) -> PromptRuntimeSnapshot {
    PromptRuntimeSnapshot::from_environment(&environment.read(), PathBuf::from("/"))
}

/// The logical PATH wins over a stale process-global PATH: the prompt sees
/// the same `node` the shell would execute.
#[test]
fn logical_path_wins_over_process_path() {
    let _lock = crate::test_env_lock();
    let logical = tempdir().unwrap();
    let process = tempdir().unwrap();
    write_executable(&logical.path().join("node"), "#!/bin/sh\necho v99.0.0\n");
    write_executable(&process.path().join("node"), "#!/bin/sh\necho v1.0.0\n");

    let environment = prompt_test_environment(&logical.path().to_string_lossy());
    let _stale_path = ProcessEnvGuard::set("PATH", &process.path().to_string_lossy());

    let runtime = runtime_for(&environment);
    assert_eq!(
        block_on_probe(fetch_node_version_async(&runtime)).as_deref(),
        Some("v99.0.0")
    );
}

/// A command that exists only on the process-global PATH is invisible to
/// the prompt: no process-global fallback.
#[test]
fn process_only_command_is_invisible() {
    let _lock = crate::test_env_lock();
    let logical = tempdir().unwrap();
    let process = tempdir().unwrap();
    write_executable(&process.path().join("node"), "#!/bin/sh\necho v1.0.0\n");

    let environment = prompt_test_environment(&logical.path().to_string_lossy());
    let _stale_path = ProcessEnvGuard::set("PATH", &process.path().to_string_lossy());

    let runtime = runtime_for(&environment);
    assert_eq!(runtime.resolve_program("node"), None);
    assert_eq!(block_on_probe(fetch_node_version_async(&runtime)), None);
}

/// A non-executable PATH candidate is skipped, and the first executable in
/// PATH order wins.
#[test]
fn nonexecutable_candidate_is_skipped_in_path_order() {
    let _lock = crate::test_env_lock();
    let dir = tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(first.join("node"), "#!/bin/sh\necho v1.0.0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(first.join("node")).unwrap().permissions();
    permissions.set_mode(0o644);
    std::fs::set_permissions(first.join("node"), permissions).unwrap();
    write_executable(&second.join("node"), "#!/bin/sh\necho v2.0.0\n");

    let path_value = format!("{}:{}", first.display(), second.display());
    let environment = prompt_test_environment(&path_value);
    let runtime = runtime_for(&environment);
    assert_eq!(
        runtime.resolve_program("node").as_deref(),
        Some(second.join("node").as_path())
    );
}

/// When every candidate is executable, PATH order decides.
#[test]
fn path_order_is_preserved() {
    let _lock = crate::test_env_lock();
    let dir = tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    write_executable(&first.join("node"), "#!/bin/sh\necho v1.0.0\n");
    write_executable(&second.join("node"), "#!/bin/sh\necho v2.0.0\n");

    let path_value = format!("{}:{}", first.display(), second.display());
    let environment = prompt_test_environment(&path_value);
    let runtime = runtime_for(&environment);
    assert_eq!(
        runtime.resolve_program("node").as_deref(),
        Some(first.join("node").as_path())
    );
}

/// A name containing `/` bypasses PATH search, like the shell's own
/// lookup: a pathname must never resolve against snapshot directories.
#[test]
fn slash_containing_name_bypasses_path_search() {
    let _lock = crate::test_env_lock();
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    write_executable(&sub.join("node"), "#!/bin/sh\necho v1.0.0\n");

    let environment = prompt_test_environment(&dir.path().to_string_lossy());
    let runtime = PromptRuntimeSnapshot::from_environment(&environment.read(), dir.path().into());
    assert_eq!(runtime.resolve_program("sub/node"), None);
    assert_eq!(runtime.resolve_program("./node"), None);
    assert!(runtime.command("sub/node").is_none());
}

/// A symlinked executable resolves and spawns through the link.
#[test]
fn symlinked_executable_is_resolved() {
    let _lock = crate::test_env_lock();
    let dir = tempdir().unwrap();
    let real = dir.path().join("real-node");
    write_executable(&real, "#!/bin/sh\necho v9.9.9\n");
    let link = dir.path().join("node");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let environment = prompt_test_environment(&dir.path().to_string_lossy());
    let runtime = runtime_for(&environment);
    assert_eq!(
        runtime.resolve_program("node").as_deref(),
        Some(link.as_path())
    );
    assert_eq!(
        block_on_probe(fetch_node_version_async(&runtime)).as_deref(),
        Some("v9.9.9")
    );
}

/// A relative PATH entry resolves against the snapshot cwd, not the
/// process cwd or any later state.
#[test]
fn relative_path_entry_resolves_against_snapshot_cwd() {
    let _lock = crate::test_env_lock();
    let root = tempdir().unwrap();
    let project_a = root.path().join("project-a");
    let project_b = root.path().join("project-b");
    std::fs::create_dir_all(project_a.join("bin")).unwrap();
    std::fs::create_dir_all(project_b.join("bin")).unwrap();
    write_executable(
        &project_a.join("bin").join("node"),
        "#!/bin/sh\necho v18.0.0\n",
    );
    write_executable(
        &project_b.join("bin").join("node"),
        "#!/bin/sh\necho v24.0.0\n",
    );

    let environment = prompt_test_environment("bin");
    let runtime = PromptRuntimeSnapshot::from_environment(&environment.read(), project_a.clone());
    assert_eq!(runtime.current_dir(), project_a.as_path());
    assert_eq!(
        runtime.resolve_program("node").as_deref(),
        Some(project_a.join("bin").join("node").as_path())
    );
    assert_eq!(
        block_on_probe(fetch_node_version_async(&runtime)).as_deref(),
        Some("v18.0.0")
    );
}

/// An exported logical variable reaches the probe child; a stale process
/// value does not leak through.
#[test]
fn child_env_carries_exported_marker() {
    let _lock = crate::test_env_lock();
    let dir = tempdir().unwrap();
    write_executable(
        &dir.path().join("docker"),
        "#!/bin/sh\nprintf '%s\\n' \"${DOGESH_PROMPT_MARKER-unset}\"\n",
    );

    let environment = prompt_test_environment(&dir.path().to_string_lossy());
    environment
        .write()
        .set_and_export_shell_var("DOGESH_PROMPT_MARKER".to_string(), "logical".to_string());
    let _stale = ProcessEnvGuard::set("DOGESH_PROMPT_MARKER", "stale");

    let runtime = runtime_for(&environment);
    let mut command = runtime
        .command("docker")
        .expect("logical docker must resolve");
    let output = block_on_probe(async move { command.output().await }).unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "logical");
}

/// A logical unset stays unset in the child even when the process
/// environment still holds the stale value.
#[test]
fn logical_unset_prevents_resurrection() {
    let _lock = crate::test_env_lock();
    let _stale = ProcessEnvGuard::set("DOGESH_PROMPT_MARKER", "stale");
    // The shell imports the stale value at startup, then the user unsets it.
    let environment = Environment::new();
    {
        let mut env = environment.write();
        env.unset_shell_var("DOGESH_PROMPT_MARKER");
    }

    let dir = tempdir().unwrap();
    write_executable(
        &dir.path().join("docker"),
        "#!/bin/sh\nprintf '%s\\n' \"${DOGESH_PROMPT_MARKER-unset}\"\n",
    );
    environment.write().set_shell_var(
        "PATH".to_string(),
        dir.path().to_string_lossy().into_owned(),
    );

    let runtime = runtime_for(&environment);
    assert!(!runtime.child_env().contains_key("DOGESH_PROMPT_MARKER"));
    let mut command = runtime
        .command("docker")
        .expect("logical docker must resolve");
    let output = block_on_probe(async move { command.output().await }).unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "unset");
}

/// A same-session PATH mutation makes `kubectl` visible immediately: no
/// process-lifetime availability cache may pin the old answer.
#[test]
fn path_mutation_makes_kubectl_visible() {
    use super::version_probes::should_attempt_k8s_context_check_with;
    let _lock = crate::test_env_lock();
    let dir = tempdir().unwrap();
    let bindir = dir.path().join("bin");
    let kubedir = dir.path().join("kubedir");
    std::fs::create_dir_all(&bindir).unwrap();
    std::fs::create_dir_all(&kubedir).unwrap();
    write_executable(&kubedir.join("kubectl"), "#!/bin/sh\nexit 0\n");
    let kubeconfig = dir.path().join("kubeconfig");
    std::fs::write(&kubeconfig, "apiVersion: v1\n").unwrap();

    let environment = prompt_test_environment(&bindir.to_string_lossy());
    environment.write().set_shell_var(
        "KUBECONFIG".to_string(),
        kubeconfig.to_string_lossy().into_owned(),
    );

    let before = runtime_for(&environment);
    assert_eq!(before.resolve_program("kubectl"), None);
    assert!(!should_attempt_k8s_context_check_with(&before));

    environment
        .write()
        .insert_path_entry(0, &kubedir.to_string_lossy());

    let after = runtime_for(&environment);
    assert_eq!(
        after.resolve_program("kubectl").as_deref(),
        Some(kubedir.join("kubectl").as_path())
    );
    assert!(should_attempt_k8s_context_check_with(&after));
}

/// Docker availability follows the logical PATH dynamically.
#[test]
fn docker_availability_is_dynamic() {
    use super::version_probes::should_attempt_docker_context_check_from;
    let _lock = crate::test_env_lock();
    let dir = tempdir().unwrap();
    let bindir = dir.path().join("bin");
    let dockdir = dir.path().join("dockdir");
    std::fs::create_dir_all(&bindir).unwrap();
    std::fs::create_dir_all(&dockdir).unwrap();
    write_executable(&dockdir.join("docker"), "#!/bin/sh\nexit 0\n");

    let environment = prompt_test_environment(&bindir.to_string_lossy());

    let before = runtime_for(&environment);
    assert!(!should_attempt_docker_context_check_from(&before));

    environment
        .write()
        .insert_path_entry(0, &dockdir.to_string_lossy());

    let after = runtime_for(&environment);
    assert!(should_attempt_docker_context_check_from(&after));
}

/// `python3` missing from the logical PATH falls back to the logical
/// `python`, never to a process-global `python3`.
#[test]
fn python_falls_back_to_logical_python() {
    let _lock = crate::test_env_lock();
    let logical = tempdir().unwrap();
    let process = tempdir().unwrap();
    write_executable(
        &logical.path().join("python"),
        "#!/bin/sh\necho 'Python 3.99.0'\n",
    );
    write_executable(
        &process.path().join("python3"),
        "#!/bin/sh\necho 'Python 1.0.0'\n",
    );

    let environment = prompt_test_environment(&logical.path().to_string_lossy());
    let _stale_path = ProcessEnvGuard::set("PATH", &process.path().to_string_lossy());

    let runtime = runtime_for(&environment);
    assert_eq!(runtime.resolve_program("python3"), None);
    assert_eq!(
        block_on_probe(fetch_python_version_async(&runtime)).as_deref(),
        Some("3.99.0")
    );
}

/// An unexported logical PATH still drives resolution, but never leaks
/// into the child environment.
#[test]
fn unexported_path_resolves_but_stays_out_of_child_env() {
    let _lock = crate::test_env_lock();
    let dir = tempdir().unwrap();
    write_executable(&dir.path().join("node"), "#!/bin/sh\necho v7.7.7\n");

    let environment = shell_only_environment();
    {
        let mut env = environment.write();
        // Drop any inherited export bit, then set the value unexported.
        env.unset_shell_var("PATH");
        env.set_shell_var(
            "PATH".to_string(),
            dir.path().to_string_lossy().into_owned(),
        );
    }

    let runtime = runtime_for(&environment);
    assert!(!runtime.child_env().contains_key("PATH"));
    assert_eq!(
        block_on_probe(fetch_node_version_async(&runtime)).as_deref(),
        Some("v7.7.7")
    );
}

fn node_project_prompt() -> (Prompt, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();
    let prompt = Prompt::new(dir.path().to_path_buf(), "🐕 < ".to_string());
    (prompt, dir)
}

/// A PATH generation change drops external-tool version/context caches but
/// keeps PATH-independent state like the AWS profile.
#[test]
fn path_generation_invalidates_version_caches() {
    let _lock = crate::test_env_lock();
    let (mut prompt, _project_dir) = node_project_prompt();
    prompt.observe_runtime_path_generation(10);
    prompt.update_node_version(Some("v18.0.0".to_string()));
    prompt.update_docker_context(Some("old-context".to_string()));
    prompt.update_aws_profile(Some("shell-profile".to_string()));
    assert!(!prompt.needs_node_check());

    prompt.observe_runtime_path_generation(11);

    assert_eq!(prompt.node_version_cache, None);
    assert_eq!(prompt.docker_context_cache, None);
    assert!(prompt.needs_node_check());
    assert_eq!(prompt.aws_profile_cache.as_deref(), Some("shell-profile"));
}

/// Re-observing the same generation keeps caches: no re-probe every tick.
#[test]
fn same_generation_keeps_caches() {
    let _lock = crate::test_env_lock();
    let (mut prompt, _project_dir) = node_project_prompt();
    prompt.observe_runtime_path_generation(10);
    prompt.update_node_version(Some("v18.0.0".to_string()));

    prompt.observe_runtime_path_generation(10);

    assert_eq!(prompt.node_version_cache.as_deref(), Some("v18.0.0"));
    assert!(!prompt.needs_node_check());
}

/// A PATH generation change resets failure backoff so a newly added tool
/// is probed immediately instead of after the old backoff delay.
#[test]
fn path_generation_resets_failure_backoff() {
    let _lock = crate::test_env_lock();
    let (mut prompt, _project_dir) = node_project_prompt();
    prompt.observe_runtime_path_generation(10);
    prompt.mark_node_check_failed();
    assert!(!prompt.needs_node_check());

    prompt.observe_runtime_path_generation(11);

    assert!(prompt.needs_node_check());
}
