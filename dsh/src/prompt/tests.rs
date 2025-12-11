use super::context::PromptContext;
use super::modules::PromptModule;
use super::modules::execution_time::ExecutionTimeModule;
use super::modules::exit_status::ExitStatusModule;
use std::path::PathBuf;
use std::time::Duration;

fn create_empty_context() -> PromptContext {
    PromptContext {
        current_dir: PathBuf::from("/"),
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
    }
}

#[test]
fn test_execution_time_module_short() {
    let module = ExecutionTimeModule::new();
    let mut context = create_empty_context();
    context.last_duration = Some(Duration::from_secs(5));

    let output = module.render(&context).unwrap();
    assert!(output.contains("5s"));
}

#[test]
fn test_execution_time_module_long() {
    let module = ExecutionTimeModule::new();
    let mut context = create_empty_context();
    context.last_duration = Some(Duration::from_secs(65));

    let output = module.render(&context).unwrap();
    assert!(output.contains("1m5s"));
}

#[test]
fn test_execution_time_module_none_under_threshold() {
    let module = ExecutionTimeModule::new();
    let mut context = create_empty_context();
    context.last_duration = Some(Duration::from_secs(1));

    assert!(module.render(&context).is_none());
}

#[test]
fn test_exit_status_module_success() {
    let module = ExitStatusModule::new();
    let mut context = create_empty_context();
    context.last_exit_status = 0;

    assert!(module.render(&context).is_none());
}

#[test]
fn test_exit_status_module_failure() {
    let module = ExitStatusModule::new();
    let mut context = create_empty_context();
    context.last_exit_status = 127;

    let output = module.render(&context).unwrap();
    assert!(output.contains("✘"));
    assert!(output.contains("127"));
}
