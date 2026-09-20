//! Pipeline isolation policy: whole-pipeline preflight and Smart Pipe data.
//!
//! Single-foreground builtins run in the parent shell; every multi-stage
//! pipeline member (first/middle/last, synthetic sources included) runs
//! isolated. Session-bound builtins and exported Lisp commands cannot be
//! reproduced in a fresh helper, so a pipeline containing one is rejected
//! before any stage spawns.

use super::materialize::CommandMaterializationFailure;
use crate::process::{Job, JobProcess};
use crate::shell::Shell;

/// Previous stdout with historical newline compatibility: stored output
/// ending in `\n` passes through, non-empty output without one gains a
/// single `\n`, empty history yields an empty source.
pub(crate) fn smart_pipe_source_data(shell: &Shell) -> String {
    let output = shell
        .environment
        .read()
        .session_output_state
        .output_history
        .last_stdout()
        .unwrap_or_default()
        .to_string();
    if output.is_empty() || output.ends_with('\n') {
        output
    } else {
        let mut with_newline = output;
        with_newline.push('\n');
        with_newline
    }
}

/// Whole-pipeline preflight: any multi-stage pipeline containing a
/// session-bound builtin (or an exported Lisp command the helper cannot
/// reproduce, or an unknown policy) is rejected before any stage spawns.
pub(crate) fn reject_session_bound_pipeline(
    job: &Job,
    shell: &Shell,
) -> Result<(), CommandMaterializationFailure> {
    let Some(head) = job.process.as_deref() else {
        return Ok(());
    };
    if head.stage_count() < 2 {
        return Ok(());
    }
    let mut current = Some(head);
    while let Some(process) = current {
        if let JobProcess::Builtin(builtin) = process {
            let requires_parent = match dsh_builtin::background_builtin_mode(&builtin.name) {
                Some(dsh_builtin::BackgroundBuiltinMode::Reexec) => false,
                Some(dsh_builtin::BackgroundBuiltinMode::ParentSessionRequired) => true,
                None => true,
            } || shell.lisp_engine.borrow().is_export(&builtin.name);
            if requires_parent {
                return Err(
                    CommandMaterializationFailure::pipeline_builtin_requires_parent(&builtin.name),
                );
            }
        }
        current = process.next_process();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{BuiltinProcess, Process};
    use dsh_types::ExitStatus;

    fn exit_zero(
        _ctx: &dsh_types::Context,
        _argv: Vec<String>,
        _proxy: &mut dyn dsh_builtin::ShellProxy,
    ) -> ExitStatus {
        ExitStatus::ExitedWith(0)
    }

    fn builtin_job(stages: Vec<JobProcess>, shell_pgid: nix::unistd::Pid) -> Job {
        let mut job = Job::new("pipeline".to_string(), shell_pgid);
        for stage in stages {
            job.set_process(stage);
        }
        job
    }

    fn reexec_builtin(name: &str) -> JobProcess {
        let handler = dsh_builtin::get_handler(name).expect("reexec builtin exists");
        JobProcess::Builtin(BuiltinProcess::new_handler(
            name.to_string(),
            handler,
            vec![name.to_string()],
        ))
    }

    fn session_builtin(name: &str) -> JobProcess {
        let handler = dsh_builtin::get_handler(name).expect("session builtin exists");
        JobProcess::Builtin(BuiltinProcess::new_handler(
            name.to_string(),
            handler,
            vec![name.to_string()],
        ))
    }

    fn external(cmd: &str) -> JobProcess {
        JobProcess::Command(Process::new(cmd.to_string(), vec![cmd.to_string()]))
    }

    #[test]
    fn single_foreground_builtin_needs_no_rejection() {
        let shell = Shell::new(crate::environment::Environment::new());
        let job = builtin_job(vec![reexec_builtin("cd")], nix::unistd::Pid::from_raw(0));
        assert!(reject_session_bound_pipeline(&job, &shell).is_ok());
    }

    #[test]
    fn pipeline_positions_all_reject_session_bound_builtins() {
        let shell = Shell::new(crate::environment::Environment::new());
        let pgid = nix::unistd::Pid::from_raw(0);
        // First, middle, and last positions each reject.
        for job in [
            builtin_job(vec![session_builtin("jobs"), external("true")], pgid),
            builtin_job(
                vec![external("true"), session_builtin("jobs"), external("true")],
                pgid,
            ),
            builtin_job(vec![external("true"), session_builtin("jobs")], pgid),
        ] {
            let err = reject_session_bound_pipeline(&job, &shell)
                .expect_err("session-bound pipeline must reject");
            assert!(err.message.contains("cannot run in a pipeline"));
            assert_eq!(err.exit_code, 1);
        }
    }

    #[test]
    fn exported_lisp_in_pipeline_rejects_like_session_bound() {
        let shell = Shell::new(crate::environment::Environment::new());
        shell
            .lisp_engine
            .borrow()
            .run("(fn isolation-lisp-probe () 1)")
            .expect("define Lisp export");
        let pgid = nix::unistd::Pid::from_raw(0);
        let lisp_stage = JobProcess::Builtin(BuiltinProcess::new(
            "isolation-lisp-probe".to_string(),
            exit_zero,
            vec!["isolation-lisp-probe".to_string()],
        ));
        let job = builtin_job(vec![external("true"), lisp_stage], pgid);
        let err =
            reject_session_bound_pipeline(&job, &shell).expect_err("Lisp pipeline must reject");
        assert!(err.message.contains("cannot run in a pipeline"));
    }

    #[test]
    fn smart_pipe_source_data_keeps_newline_compat() {
        use dsh_types::output_history::OutputEntry;
        let shell = Shell::new(crate::environment::Environment::new());
        // Empty history yields an empty source.
        assert_eq!(smart_pipe_source_data(&shell), "");
        // Newline-terminated output passes through.
        shell
            .environment
            .write()
            .session_output_state
            .output_history
            .push(OutputEntry::new("cmd".into(), "abc\n".into(), "".into(), 0));
        assert_eq!(smart_pipe_source_data(&shell), "abc\n");
        // Non-empty output without a newline gains exactly one.
        shell
            .environment
            .write()
            .session_output_state
            .output_history
            .push(OutputEntry::new("cmd".into(), "abc".into(), "".into(), 0));
        assert_eq!(smart_pipe_source_data(&shell), "abc\n");
    }
}
