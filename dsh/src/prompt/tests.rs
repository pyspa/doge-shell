use super::context::PromptContext;
use super::modules::PromptModule;
use super::modules::execution_time::ExecutionTimeModule;
use super::modules::exit_status::ExitStatusModule;
use std::path::PathBuf;
use std::time::Duration;

#[test]
fn test_execution_time_module_short() {
    let module = ExecutionTimeModule::new();
    let current_dir = PathBuf::from("/");
    let context = PromptContext {
        current_dir: &current_dir,
        git_root: None,
        git_status: None,
        rust_version: None,
        node_version: None,
        python_version: None,
        go_version: None,
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
        git_root: None,
        git_status: None,
        rust_version: None,
        node_version: None,
        python_version: None,
        go_version: None,
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
        git_root: None,
        git_status: None,
        rust_version: None,
        node_version: None,
        python_version: None,
        go_version: None,
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
        git_root: None,
        git_status: None,
        rust_version: None,
        node_version: None,
        python_version: None,
        go_version: None,
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
        git_root: None,
        git_status: None,
        rust_version: None,
        node_version: None,
        python_version: None,
        go_version: None,
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
