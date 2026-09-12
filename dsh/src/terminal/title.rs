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

/// The basename of the last external command in `job`'s pipeline (`None` for
/// a builtin-only job), unsanitized. Used by
/// `crate::agent_lifecycle::yield_to_foreground_agent` to decide whether a
/// foreground job is a Herdr-recognized agent CLI - the same "last external
/// process in the pipeline" rule `command_title` uses for the terminal
/// title, since a command that only feeds a pipe never draws to the
/// terminal for Herdr's own screen detection to see either.
pub(crate) fn last_external_command_basename(job: &Job) -> Option<String> {
    let process = job.process.as_ref()?;
    let name = last_external_process_name(process)?;
    Some(basename(&name).into_owned())
}

/// The pipeline's true last stage's name, if that stage is an external
/// command (`None` if it's a builtin). Only ever looks at the actual last
/// stage - it must not fall back to naming an earlier stage just because
/// nothing further along resolved: for `codex | cd`, the pipeline's last
/// stage is the builtin `cd`, so this returns `None` even though `codex`
/// (an earlier stage) is a `Command`. `command_title`'s own fallback to
/// `first_token(&job.cmd)` still shows something reasonable in the title
/// bar for that case; `last_external_command_basename`'s callers rely on
/// `None` meaning "the last stage draws nothing of its own to the
/// terminal" - a fallback to `codex` here would be simply wrong for that
/// use.
fn last_external_process_name(process: &JobProcess) -> Option<String> {
    match process.next() {
        Some(next) => last_external_process_name(&next),
        None => match process {
            JobProcess::Command(_) => Some(process.get_cmd().to_string()),
            JobProcess::Builtin(_) => None,
        },
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
    use super::{command_title, last_external_command_basename};
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

    #[test]
    fn last_external_command_basename_uses_absolute_path_basename() {
        let job = job_with_process(JobProcess::Command(Process::new(
            "/usr/local/bin/codex".to_string(),
            vec!["/usr/local/bin/codex".to_string()],
        )));

        assert_eq!(
            last_external_command_basename(&job).as_deref(),
            Some("codex")
        );
    }

    #[test]
    fn last_external_command_basename_uses_last_process_in_pipeline() {
        let mut first = Process::new(
            "git".to_string(),
            vec!["git".to_string(), "log".to_string()],
        );
        first.link(JobProcess::Command(Process::new(
            "codex".to_string(),
            vec!["codex".to_string()],
        )));
        let job = job_with_process(JobProcess::Command(first));

        assert_eq!(
            last_external_command_basename(&job).as_deref(),
            Some("codex")
        );
    }

    #[test]
    fn last_external_command_basename_is_none_for_builtin_only_input() {
        let job = job_with_process(JobProcess::Builtin(BuiltinProcess::new(
            "cd".to_string(),
            dummy_builtin,
            vec!["cd".to_string(), "/tmp".to_string()],
        )));

        assert_eq!(last_external_command_basename(&job), None);
    }

    #[test]
    fn last_external_command_basename_is_none_when_the_pipeline_ends_in_a_builtin() {
        // Regression test: `codex | cd` must not report "codex" as the
        // pipeline's last command. The builtin `cd` is the actual last
        // stage and draws nothing to the terminal itself, but neither does
        // `codex` here - its output feeds `cd`, not the screen - so this
        // must not be mistaken for a foreground agent CLI actually running
        // (see `agent_lifecycle::yield_to_foreground_agent`, the consumer
        // this distinction matters for).
        let mut first = Process::new("codex".to_string(), vec!["codex".to_string()]);
        first.link(JobProcess::Builtin(BuiltinProcess::new(
            "cd".to_string(),
            dummy_builtin,
            vec!["cd".to_string(), "/tmp".to_string()],
        )));
        let job = job_with_process(JobProcess::Command(first));

        assert_eq!(last_external_command_basename(&job), None);
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
