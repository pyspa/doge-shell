use crate::prompt::GitStatus;
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct PromptContext<'a> {
    pub current_dir: &'a Path,
    pub git_root: Option<&'a Path>,
    pub git_status: Option<&'a GitStatus>,
    pub rust_version: Option<&'a str>,
    pub node_version: Option<&'a str>,
    pub python_version: Option<&'a str>,
    pub go_version: Option<&'a str>,
    pub k8s_context: Option<&'a str>,
    pub k8s_namespace: Option<&'a str>,
    pub aws_profile: Option<&'a str>,
    pub docker_context: Option<&'a str>,
    pub last_exit_status: i32,
    pub last_duration: Option<std::time::Duration>,
}
