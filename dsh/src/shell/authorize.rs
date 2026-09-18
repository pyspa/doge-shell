//! One authorization funnel for every job: top-level lines and nested bodies.
//!
//! The guard judges the materialized job; the prompt lives here so nested
//! substitution paths cannot drift into a second copy of it.

use crate::repl::confirmation::ConfirmationAction;
use crate::safety::{SafetyCheckContext, SafetyResult};
use crate::shell::Shell;
use anyhow::Result;

pub type ConfirmFn = fn(&str) -> Result<ConfirmationAction>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationDecision {
    Allow,
    Deny,
}

#[derive(Debug)]
pub struct AuthorizationCancelled;

impl std::fmt::Display for AuthorizationCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "command authorization was denied")
    }
}

impl std::error::Error for AuthorizationCancelled {}

pub fn is_authorization_cancelled(err: &anyhow::Error) -> bool {
    err.downcast_ref::<AuthorizationCancelled>().is_some()
}

pub fn authorize_job(
    shell: &mut Shell,
    job: &crate::process::Job,
    had_dynamic: bool,
) -> Result<AuthorizationDecision> {
    authorize_job_with(
        shell,
        job,
        had_dynamic,
        crate::repl::confirmation::confirm_action,
    )
}

pub fn authorize_job_with(
    shell: &mut Shell,
    job: &crate::process::Job,
    had_dynamic: bool,
    confirm: ConfirmFn,
) -> Result<AuthorizationDecision> {
    let (level, allowlist) = {
        let env = shell.environment.read();
        let level = *env.policy_state.safety_level.read();
        let mut allowlist = env.policy_state.execute_allowlist.read().clone();
        allowlist.extend(
            env.policy_state
                .shell_always_allowlist
                .read()
                .iter()
                .cloned(),
        );
        (level, allowlist)
    };
    let ctx = if had_dynamic {
        SafetyCheckContext::dynamic_source()
    } else {
        SafetyCheckContext::strict_source()
    };
    match shell.safety_guard.check_jobs_with_context(
        std::slice::from_ref(job),
        &level,
        &allowlist,
        &ctx,
    ) {
        SafetyResult::Allowed => Ok(AuthorizationDecision::Allow),
        SafetyResult::Confirm(reason) => match confirm(&reason) {
            Ok(ConfirmationAction::Yes) => Ok(AuthorizationDecision::Allow),
            Ok(ConfirmationAction::AlwaysAllow) => {
                // A dynamic source can resolve differently next time.
                if !had_dynamic {
                    shell
                        .environment
                        .read()
                        .policy_state
                        .shell_always_allowlist
                        .write()
                        .push(job.cmd.clone());
                }
                Ok(AuthorizationDecision::Allow)
            }
            Ok(ConfirmationAction::No) | Err(_) => Ok(AuthorizationDecision::Deny),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Job, JobProcess, Process};
    use nix::unistd::Pid;

    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    fn deny_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::No)
    }

    fn always_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::AlwaysAllow)
    }

    fn shell() -> Shell {
        Shell::new(crate::environment::Environment::new())
    }

    fn concrete_job(cmd: &str, argv: &[&str]) -> Job {
        let mut job = Job::new(cmd.to_string(), Pid::from_raw(0));
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let program = argv.first().cloned().unwrap_or_default();
        job.set_process(JobProcess::Command(Process::new(program, argv)));
        job
    }

    /// Test H (unit): denying the inner body denies the whole chain. The
    /// materializer maps this to 130 and never launches the outer command.
    #[test]
    fn a_denied_body_denies_authorization() {
        let mut shell = shell();
        let job = concrete_job("rm -rf /tmp/dogesh_nested_probe", &["rm", "-rf"]);
        assert_eq!(
            authorize_job_with(&mut shell, &job, true, deny_all).unwrap(),
            AuthorizationDecision::Deny
        );
        assert_eq!(
            authorize_job_with(&mut shell, &job, false, allow_all).unwrap(),
            AuthorizationDecision::Allow
        );
    }

    /// `AlwaysAllow` on a dynamic source is single-run: it must not land in
    /// the session allowlist where it would pre-approve future resolutions.
    #[test]
    fn always_allow_on_dynamic_source_is_not_persisted() {
        let mut shell = shell();
        let job = concrete_job(
            "rm -rf /tmp/dogesh_always_probe",
            &["rm", "-rf", "/tmp/dogesh_always_probe"],
        );
        assert_eq!(
            authorize_job_with(&mut shell, &job, false, always_all).unwrap(),
            AuthorizationDecision::Allow
        );
        assert!(
            shell
                .environment
                .read()
                .policy_state
                .shell_always_allowlist
                .read()
                .contains(&job.cmd)
        );

        let dynamic = concrete_job(
            "$(printf rm) -rf /tmp/dogesh_always_probe",
            &["rm", "-rf", "/tmp/dogesh_always_probe"],
        );
        assert_eq!(
            authorize_job_with(&mut shell, &dynamic, true, always_all).unwrap(),
            AuthorizationDecision::Allow
        );
        assert!(
            !shell
                .environment
                .read()
                .policy_state
                .shell_always_allowlist
                .read()
                .contains(&dynamic.cmd),
            "dynamic AlwaysAllow must not persist the raw source"
        );
    }
}
