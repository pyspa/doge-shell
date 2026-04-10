use crate::process::{Job, JobProcess};
use std::borrow::Cow;
use std::io::{self, Write};
use std::path::Path;

const DEFAULT_TITLE: &str = "dsh";
const MAX_TITLE_CHARS: usize = 64;

pub fn set_running_title(job: &Job) -> io::Result<()> {
    write_title(&command_title(job))
}

pub fn reset_title() -> io::Result<()> {
    write_title(DEFAULT_TITLE)
}

fn write_title(title: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    write!(stdout, "\x1b]0;{title}\x07\x1b]2;{title}\x07")?;
    stdout.flush()
}

fn command_title(job: &Job) -> String {
    if let Some(process) = job.process.as_ref()
        && let Some(name) = last_external_process_name(process)
    {
        return sanitize_title(&name);
    }

    sanitize_title(first_token(&job.cmd))
}

fn last_external_process_name(process: &JobProcess) -> Option<String> {
    let next_external = process
        .next()
        .and_then(|next| last_external_process_name(&next));
    if next_external.is_some() {
        return next_external;
    }

    match process {
        JobProcess::Command(_) => Some(process.get_cmd().to_string()),
        JobProcess::Builtin(_) => None,
    }
}

fn first_token(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or(DEFAULT_TITLE)
}

fn sanitize_title(raw: &str) -> String {
    let base = basename(raw);
    let filtered: String = base.chars().filter(|ch| !ch.is_control()).collect();
    let trimmed = filtered.trim();
    if trimmed.is_empty() {
        return DEFAULT_TITLE.to_string();
    }

    let truncated: String = trimmed.chars().take(MAX_TITLE_CHARS).collect();
    if truncated.is_empty() {
        DEFAULT_TITLE.to_string()
    } else {
        truncated
    }
}

fn basename(raw: &str) -> Cow<'_, str> {
    let path = Path::new(raw);
    if let Some(name) = path.file_name().and_then(|value| value.to_str())
        && !name.is_empty()
    {
        return Cow::Borrowed(name);
    }

    Cow::Borrowed(raw)
}

#[cfg(test)]
mod tests {
    use super::command_title;
    use crate::process::{BuiltinProcess, Job, JobProcess, Process};
    use nix::unistd::getpgrp;

    #[test]
    fn title_uses_command_name_for_simple_command() {
        let job = job_with_process(JobProcess::Command(Process::new(
            "git".to_string(),
            vec!["git".to_string(), "status".to_string()],
        )));

        assert_eq!(command_title(&job), "git");
    }

    #[test]
    fn title_uses_basename_for_absolute_command_path() {
        let job = job_with_process(JobProcess::Command(Process::new(
            "/usr/bin/nvim".to_string(),
            vec!["/usr/bin/nvim".to_string(), "foo.txt".to_string()],
        )));

        assert_eq!(command_title(&job), "nvim");
    }

    #[test]
    fn title_uses_last_external_process_in_pipeline() {
        let mut first = Process::new("rg".to_string(), vec!["rg".to_string(), "foo".to_string()]);
        first.link(JobProcess::Command(Process::new(
            "less".to_string(),
            vec!["less".to_string()],
        )));
        let job = job_with_process(JobProcess::Command(first));

        assert_eq!(command_title(&job), "less");
    }

    #[test]
    fn title_falls_back_to_first_token_for_builtin_only_input() {
        let job = job_with_process(JobProcess::Builtin(BuiltinProcess::new(
            "cd".to_string(),
            dummy_builtin,
            vec!["cd".to_string(), "/tmp".to_string()],
        )));

        assert_eq!(command_title(&job), "cd");
    }

    #[test]
    fn title_removes_control_characters() {
        let job = Job::new("printf \u{1b}[31mred".to_string(), getpgrp());
        assert_eq!(command_title(&job), "printf");
    }

    #[test]
    fn title_truncates_long_values() {
        let long_name = format!("{}tail", "a".repeat(80));
        let job = job_with_process(JobProcess::Command(Process::new(
            long_name.clone(),
            vec![long_name],
        )));

        assert_eq!(command_title(&job), "a".repeat(64));
    }

    fn job_with_process(process: JobProcess) -> Job {
        let mut job = Job::new(process.get_cmd().to_string(), getpgrp());
        job.process = Some(Box::new(process));
        job
    }

    fn dummy_builtin(
        _ctx: &dsh_types::Context,
        _argv: Vec<String>,
        _proxy: &mut dyn dsh_builtin::ShellProxy,
    ) -> dsh_types::ExitStatus {
        dsh_types::ExitStatus::ExitedWith(0)
    }
}
