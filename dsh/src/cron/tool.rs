//! `impl CronToolHost for Shell`: the other half of the `cron_manage` chat
//! tool (`dsh-builtin/src/chatgpt/tool/cron.rs`).
//!
//! Every action here turns a [`CronToolRequest`]'s already-typed-but-unparsed
//! fields into the exact argv `cron add`/`cron edit` would have parsed, and
//! hands it to [`parse_add`]/[`parse_edit`] - so a schedule typo or a
//! duplicate job name fails with the very same message a person typing the
//! command would see. Nothing about validation is reimplemented for this
//! third entry point (the CLI and `config.lisp`'s `cron-add` being the other
//! two).
//!
//! Read actions return the same shapes `cron ... --json` already builds
//! (`super::handlers`'s json helpers); write actions return a short summary of
//! what changed. `dsh-builtin`'s side of this tool has already asked the user
//! (or, under a task, already turned a write action into `InputRequired`)
//! before any of this runs.

use anyhow::{Context as _, Result};
use dsh_builtin::config_paths;
use dsh_builtin::shell_capabilities::{CronStore, CronToolHost};
use dsh_types::agent::TaskGrant;
use dsh_types::cron::job::{RunQuery, RunSelector};
use dsh_types::cron::tool::{CronToolAction, CronToolRequest};
use serde_json::json;

use super::cli::{build_spec, current_dir_string, parse_add, parse_edit};
use super::handlers::{
    doctor_report, health_json, incident_json, job_detail_json, job_json, run_json,
};
use super::store::SqliteCronStore;
use crate::shell::Shell;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl CronToolHost for Shell {
    fn cron_tool_call(&mut self, request: &CronToolRequest) -> Result<serde_json::Value> {
        let action = request
            .action
            .context("cron_manage: `action` is required")?;
        let store = SqliteCronStore::open(&config_paths::cron_state_dir())?;

        match action {
            CronToolAction::List => list(&store),
            CronToolAction::Show => show(&store, request),
            CronToolAction::History => history(&store, request),
            CronToolAction::Logs => logs(self, &store, request),
            CronToolAction::Incidents => incidents(&store),
            CronToolAction::Status => status(&store),
            CronToolAction::Doctor => doctor(self, &store),
            CronToolAction::Create => create(self, &store, request),
            CronToolAction::Update => update(&store, request),
            CronToolAction::Pause => set_paused(&store, request, true),
            CronToolAction::Resume => set_paused(&store, request, false),
            CronToolAction::Remove => remove(&store, request),
            CronToolAction::Run => run(&store, request),
            CronToolAction::Ack => ack(&store, request),
        }
    }
}

fn list(store: &SqliteCronStore) -> Result<serde_json::Value> {
    Ok(json!(
        store.list()?.iter().map(job_json).collect::<Vec<_>>()
    ))
}

fn show(store: &SqliteCronStore, request: &CronToolRequest) -> Result<serde_json::Value> {
    let selector = request.job.as_deref().context("`job` is required")?;
    let job = store.get(selector)?;
    let notepad_path = store.notepad_path(&job.name);
    Ok(job_detail_json(&job, &notepad_path))
}

fn parse_limit(value: Option<&str>, default: usize) -> Result<usize> {
    match value {
        None => Ok(default),
        Some(text) => text
            .parse()
            .with_context(|| format!("`limit` must be a number, not {text:?}")),
    }
}

fn history(store: &SqliteCronStore, request: &CronToolRequest) -> Result<serde_json::Value> {
    let runs = store.runs(&RunQuery {
        job: request.job.clone(),
        limit: parse_limit(request.limit.as_deref(), 20)?,
        finished_only: true,
        failed_only: request.failed,
    })?;
    Ok(json!(runs.iter().map(run_json).collect::<Vec<_>>()))
}

/// [`job_cwd_within`]'s outcome - kept distinct from a plain `bool` so the
/// caller's refusal message can say *why*: an unresolvable `cwd` (deleted,
/// moved) is not the same situation as a `cwd` that genuinely falls outside
/// the grant, and conflating the two into one message sends whoever is
/// debugging the refusal chasing the wrong fix (widening a grant that was
/// never the problem).
enum CwdCheck {
    InGrant,
    OutOfGrant,
    Unresolvable,
}

/// Whether a job's `cwd` falls under a task's own grant.
///
/// The same boundary `grant_exceeds_task`
/// (`dsh-builtin/src/chatgpt/tool/cron.rs`) enforces for every *write*
/// action, applied here to a *read* one: `history` only ever shows a
/// 120-character preview of a run, but `logs` can return a job's full 8 KiB
/// of recorded output, so a task must not be able to read an unrelated
/// job's output just because it happens to know (or has guessed) the job's
/// name. Readable is the weaker claim, so either root list satisfies it -
/// the same reasoning `grant_exceeds_task` uses for its own `--read` check.
fn job_cwd_within(cwd: &str, grant: &TaskGrant) -> CwdCheck {
    match std::path::Path::new(cwd).canonicalize() {
        Ok(path) => {
            let within = grant
                .read_roots
                .iter()
                .chain(&grant.write_roots)
                .any(|root| path.starts_with(root));
            if within {
                CwdCheck::InGrant
            } else {
                CwdCheck::OutOfGrant
            }
        }
        // Refuse instead of failing open: `logs` returns the job's output
        // directly with no later check, so failing open would let a job
        // whose `cwd` no longer resolves (deleted, moved) bypass the grant
        // check entirely.
        Err(_) => CwdCheck::Unresolvable,
    }
}

fn logs(
    shell: &Shell,
    store: &SqliteCronStore,
    request: &CronToolRequest,
) -> Result<serde_json::Value> {
    let selector = match (&request.run, &request.job) {
        // Both given: the run must belong to the named job - see
        // `RunSelector::JobAndId`'s own doc comment for why `job` cannot
        // just be dropped once `run` pins down a run by itself.
        (Some(run), Some(job)) => RunSelector::JobAndId {
            job: job.clone(),
            run: run.clone(),
        },
        (Some(run), None) => RunSelector::Id(run.clone()),
        (None, Some(job)) => RunSelector::Latest(job.clone()),
        (None, None) => anyhow::bail!("`job` or `run` is required"),
    };
    let output = store.run_output(&selector)?;

    if let Some(runtime) = &shell.agent_runtime {
        let grant = runtime.lock().task.grant.clone();
        let job = store.get(&output.run.job_name)?;
        match job_cwd_within(&job.cwd, &grant) {
            CwdCheck::InGrant => {}
            CwdCheck::OutOfGrant => {
                anyhow::bail!(
                    "cron_manage: refused: job `{}` is outside this task's own read/write grant",
                    job.name
                );
            }
            CwdCheck::Unresolvable => {
                anyhow::bail!(
                    "cron_manage: refused: job `{}`'s cwd could not be resolved (deleted or moved?), \
                     so its place inside this task's grant cannot be confirmed",
                    job.name
                );
            }
        }
    }

    Ok(super::handlers::logs_json(&output, true, true))
}

fn incidents(store: &SqliteCronStore) -> Result<serde_json::Value> {
    Ok(json!(
        store
            .incidents(true, 50)?
            .iter()
            .map(incident_json)
            .collect::<Vec<_>>()
    ))
}

fn status(store: &SqliteCronStore) -> Result<serde_json::Value> {
    Ok(health_json(&store.health(now())?, store.root()))
}

fn doctor(shell: &mut Shell, store: &SqliteCronStore) -> Result<serde_json::Value> {
    Ok(doctor_report(shell, store, now())?.to_json())
}

fn create(
    shell: &mut Shell,
    store: &SqliteCronStore,
    request: &CronToolRequest,
) -> Result<serde_json::Value> {
    let mut argv: Vec<String> = Vec::new();
    if let Some(name) = &request.name {
        argv.push("--name".to_string());
        argv.push(name.clone());
    }
    if let Some(cwd) = &request.cwd {
        argv.push("--cwd".to_string());
        argv.push(cwd.clone());
    }
    if let Some(on) = &request.on {
        argv.push("--on".to_string());
        argv.push(on.clone());
    }
    if let Some(timeout) = &request.timeout {
        argv.push("--timeout".to_string());
        argv.push(timeout.clone());
    }
    if let Some(catchup) = &request.catchup {
        argv.push("--catchup".to_string());
        argv.push(catchup.clone());
    }
    if request.force {
        argv.push("--force".to_string());
    }
    // A job the tool creates always starts paused - see the module doc on the
    // `dsh-builtin` side. A person resumes it after checking a first
    // `cron run --now`.
    argv.push("--paused".to_string());
    if request.agent
        || request.goal.is_some()
        || !request.check.is_empty()
        || request.max_tokens_per_day.is_some()
        || !request.read.is_empty()
        || !request.write.is_empty()
        || !request.allow_command.is_empty()
        || !request.allow_mcp.is_empty()
        || !request.network.is_empty()
        || !request.env.is_empty()
        || request.sandbox
    {
        anyhow::bail!("agent jobs are no longer supported; create a shell job with `command`");
    }

    let schedule = request.schedule.clone().context("`schedule` is required")?;
    argv.push(schedule);
    argv.push("--".to_string());
    argv.push(
        request
            .command
            .clone()
            .context("`command` is required for a shell job")?,
    );

    let parsed = parse_add(&argv).map_err(anyhow::Error::msg)?;
    let force = parsed.force;
    let cwd = current_dir_string()?;
    let spec = build_spec(parsed, cwd).map_err(anyhow::Error::msg)?;
    let name = spec.name.clone();
    let environment = shell.environment.read().child_process_env();
    let id = store.create(&spec, &environment, now(), force)?;

    Ok(json!({
        "action": "create",
        "id": id,
        "job": name,
        "paused": true,
        "note": "created paused; a person must run `cron resume` (after checking `cron run --now`) before this job ever fires",
    }))
}

fn update(store: &SqliteCronStore, request: &CronToolRequest) -> Result<serde_json::Value> {
    let selector = request.job.clone().context("`job` is required")?;
    if request.agent
        || request.goal.is_some()
        || !request.check.is_empty()
        || request.max_tokens_per_day.is_some()
        || !request.read.is_empty()
        || !request.write.is_empty()
        || !request.allow_command.is_empty()
        || !request.allow_mcp.is_empty()
        || !request.network.is_empty()
        || !request.env.is_empty()
        || request.sandbox
    {
        anyhow::bail!("agent jobs are no longer supported");
    }
    let _ = store.get(&selector)?;

    let mut argv: Vec<String> = vec![selector];
    if let Some(name) = &request.name {
        argv.push("--name".to_string());
        argv.push(name.clone());
    }
    if let Some(schedule) = &request.schedule {
        argv.push("--schedule".to_string());
        argv.push(schedule.clone());
    }
    if let Some(command) = &request.command {
        argv.push("--command".to_string());
        argv.push(command.clone());
    }
    if let Some(cwd) = &request.cwd {
        argv.push("--cwd".to_string());
        argv.push(cwd.clone());
    }
    if let Some(on) = &request.on {
        argv.push("--on".to_string());
        argv.push(on.clone());
    }
    if let Some(timeout) = &request.timeout {
        argv.push("--timeout".to_string());
        argv.push(timeout.clone());
    }
    if let Some(catchup) = &request.catchup {
        argv.push("--catchup".to_string());
        argv.push(catchup.clone());
    }
    let (name, patch) = parse_edit(&argv).map_err(anyhow::Error::msg)?;
    let name = store.patch(&name, &patch, now())?;
    Ok(json!({ "action": "update", "job": name }))
}

fn set_paused(
    store: &SqliteCronStore,
    request: &CronToolRequest,
    paused: bool,
) -> Result<serde_json::Value> {
    let selector = request.job.clone().context("`job` is required")?;
    let name = store.set_paused(&selector, paused, now())?;
    Ok(json!({
        "action": if paused { "pause" } else { "resume" },
        "job": name,
    }))
}

fn remove(store: &SqliteCronStore, request: &CronToolRequest) -> Result<serde_json::Value> {
    let selector = request.job.clone().context("`job` is required")?;
    let name = store.delete(&selector)?;
    Ok(json!({ "action": "remove", "job": name }))
}

fn run(store: &SqliteCronStore, request: &CronToolRequest) -> Result<serde_json::Value> {
    let selector = request.job.clone().context("`job` is required")?;
    let name = store.trigger(&selector, now())?;
    Ok(json!({
        "action": "run",
        "job": name,
        "note": "marked due for the next tick (the session runner or an external `cron tick`, \
                 usually within about a minute) - this does not run it synchronously; check \
                 `history` for the outcome",
    }))
}

fn ack(store: &SqliteCronStore, request: &CronToolRequest) -> Result<serde_json::Value> {
    let raw = request
        .incident_id
        .as_deref()
        .context("`incident_id` is required")?;
    let id: i64 = raw
        .parse()
        .with_context(|| format!("`incident_id` must be a number, not {raw:?}"))?;
    let incident = store.ack_incident(id, now())?;
    Ok(json!({
        "action": "ack",
        "incident_id": incident.id,
        "kind": incident.kind.as_str(),
    }))
}

#[cfg(test)]
mod tests;
