//! Prepare structured-pipe arguments and authorize the resulting job together.

use crate::process::Job;
use crate::shell::{
    Shell,
    authorize::{AuthorizationDecision, ConfirmFn, authorize_job_with},
};
use dsh_types::output_schema::OutputSpec;

pub(super) fn authorize_struct_pipe_job(
    shell: &mut Shell,
    job: &mut Job,
    had_dynamic: bool,
    confirm: ConfirmFn,
    lookup: impl FnOnce(&[String]) -> Option<OutputSpec>,
) -> anyhow::Result<(AuthorizationDecision, Option<OutputSpec>)> {
    let spec = if !job.capture_output && !job.struct_pipe_exprs.is_empty() {
        job.last_external_argv().and_then(|argv| lookup(&argv))
    } else {
        None
    };
    let injected = spec
        .as_ref()
        .and_then(|spec| spec.prefer.as_ref())
        .is_some_and(|prefer| !prefer.inject_args.is_empty());
    if let Some(prefer) = spec.as_ref().and_then(|spec| spec.prefer.as_ref()) {
        job.append_args_to_last_external(&prefer.inject_args);
    }
    // Keep the source identity for history/hooks. A schema is mutable input,
    // so a source allowlist entry cannot approve its current or future flags.
    let decision = authorize_job_with(shell, job, had_dynamic || injected, |reason| {
        if injected {
            let mut stages = Vec::new();
            let mut stage = job.process.as_deref();
            while let Some(process) = stage {
                if let Some((program, args)) = process.command_argv() {
                    // Debug strings preserve argument boundaries and escape
                    // control characters in schema-provided values.
                    stages.push(format!("{program:?} {args:?}"));
                }
                stage = process.next_process();
            }
            confirm(&format!(
                "{reason}\nSchema-adjusted argv: {}",
                stages.join(" | ")
            ))
        } else {
            confirm(reason)
        }
    })?;
    Ok((decision, spec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        environment::Environment,
        process::{JobProcess, Process},
        repl::confirmation::ConfirmationAction,
        safety::SafetyLevel,
    };
    use dsh_types::output_schema::{ParseMode, PreferSpec};
    use nix::unistd::Pid;

    fn job() -> Job {
        let mut job = Job::new("rm target |: $_".into(), Pid::from_raw(0));
        job.set_process(JobProcess::Command(Process::new(
            "rm".into(),
            vec!["rm".into(), "target".into()],
        )));
        job.struct_pipe_exprs.push("$_".into());
        job
    }

    fn schema(_: &[String]) -> Option<OutputSpec> {
        Some(OutputSpec {
            subcommand: None,
            when: None,
            prefer: Some(PreferSpec {
                inject_args: vec!["-rf".into()],
                parse: ParseMode::Json,
                json_root: None,
            }),
            text: None,
        })
    }

    fn deny(reason: &str) -> anyhow::Result<ConfirmationAction> {
        assert!(
            reason.contains("-rf"),
            "confirmation omitted injected argv: {reason}"
        );
        Ok(ConfirmationAction::No)
    }

    #[test]
    fn schema_injection_is_authorized_despite_exact_source_allowlist() {
        for level in [SafetyLevel::Normal, SafetyLevel::Strict] {
            let mut shell = Shell::new(Environment::new());
            let mut job = job();
            *shell.environment.read().policy_state.safety_level.write() = level;
            shell
                .environment
                .read()
                .policy_state
                .execute_allowlist
                .write()
                .push(job.cmd.clone());
            let (decision, _) =
                authorize_struct_pipe_job(&mut shell, &mut job, false, deny, schema).unwrap();
            assert_eq!(decision, AuthorizationDecision::Deny, "{level:?}");
            assert_eq!(job.cmd, "rm target |: $_");
        }
    }

    fn always(_: &str) -> anyhow::Result<ConfirmationAction> {
        Ok(ConfirmationAction::AlwaysAllow)
    }

    fn unexpected_prompt(reason: &str) -> anyhow::Result<ConfirmationAction> {
        panic!("unexpected confirmation: {reason}")
    }

    #[test]
    fn schema_always_allow_does_not_preapprove_later_injection() {
        for level in [SafetyLevel::Normal, SafetyLevel::Strict] {
            let mut shell = Shell::new(Environment::new());
            *shell.environment.read().policy_state.safety_level.write() = level;
            let mut first = job();
            assert_eq!(
                authorize_struct_pipe_job(&mut shell, &mut first, false, always, schema)
                    .unwrap()
                    .0,
                AuthorizationDecision::Allow
            );
            assert!(
                shell
                    .environment
                    .read()
                    .policy_state
                    .shell_always_allowlist
                    .read()
                    .is_empty()
            );
            let mut next = job();
            // Existing raw-source AlwaysAllow entries are also insufficient.
            shell
                .environment
                .read()
                .policy_state
                .shell_always_allowlist
                .write()
                .push(next.cmd.clone());
            assert_eq!(
                authorize_struct_pipe_job(&mut shell, &mut next, false, deny, schema)
                    .unwrap()
                    .0,
                AuthorizationDecision::Deny
            );
        }
    }

    #[test]
    fn unchanged_jobs_keep_exact_source_approval() {
        for level in [SafetyLevel::Normal, SafetyLevel::Strict] {
            for mode in 0..4 {
                let mut shell = Shell::new(Environment::new());
                *shell.environment.read().policy_state.safety_level.write() = level;
                let mut job = job();
                if mode == 1 {
                    job.capture_output = true;
                }
                if mode == 2 {
                    job.struct_pipe_exprs.clear();
                }
                let before = job.last_external_argv();
                shell
                    .environment
                    .read()
                    .policy_state
                    .execute_allowlist
                    .write()
                    .push(job.cmd.clone());
                let (decision, matched) = authorize_struct_pipe_job(
                    &mut shell,
                    &mut job,
                    false,
                    unexpected_prompt,
                    |argv| {
                        assert_eq!(argv, &["rm", "target"]);
                        match mode {
                            0 => None,
                            3 => {
                                let mut spec = schema(argv).unwrap();
                                spec.prefer.as_mut().unwrap().inject_args.clear();
                                Some(spec)
                            }
                            _ => panic!("capture/ordinary job must not look up a schema"),
                        }
                    },
                )
                .unwrap();
                assert_eq!(decision, AuthorizationDecision::Allow);
                assert_eq!(job.last_external_argv(), before);
                assert_eq!(matched.is_some(), mode == 3);
            }
        }
    }

    #[test]
    fn schema_lookup_and_parser_share_the_pre_injection_spec() {
        let mut shell = Shell::new(Environment::new());
        let mut job = job();
        // A real pipeline: injection affects only the external tail and must
        // precede an operand terminator.
        let mut head = JobProcess::Command(Process::new(
            "printf".into(),
            vec!["printf".into(), "data".into()],
        ));
        head.link(JobProcess::Command(Process::new(
            "rm".into(),
            vec!["rm".into(), "--".into(), "target".into()],
        )));
        job.process = Some(Box::new(head));
        let (decision, spec) =
            authorize_struct_pipe_job(&mut shell, &mut job, false, always, |argv| {
                assert_eq!(argv, &["rm", "--", "target"]);
                schema(argv)
            })
            .unwrap();
        assert_eq!(decision, AuthorizationDecision::Allow);
        assert_eq!(
            job.last_external_argv().unwrap(),
            ["rm", "-rf", "--", "target"]
        );
        assert_eq!(
            job.process.as_deref().unwrap().command_argv().unwrap().1,
            &["data"]
        );
        let table =
            crate::output_schema::parse_with_spec(&spec.unwrap(), r#"[{"value":1}]"#).unwrap();
        assert_eq!(table.columns, ["value"]);
        assert_eq!(job.cmd, "rm target |: $_");
    }
}
