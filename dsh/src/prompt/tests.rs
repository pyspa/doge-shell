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
