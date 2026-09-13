//! `impl CronToolHost for Shell`: the other half of the `cron_manage` chat
//! tool (`dsh-builtin/src/chatgpt/tool/cron.rs`).
//!
//! Every action here turns a [`CronToolRequest`]'s already-typed-but-unparsed
//! fields into the exact argv `cron add`/`cron edit` would have parsed, and
//! hands it to [`parse_add`]/[`parse_edit`] - so a schedule typo, a grant that
//! names something that is not a directory, or a duplicate job name fails
//! with the very same message a person typing the command would see. Nothing
//! about validation is reimplemented for this third entry point (the CLI and
//! `config.lisp`'s `cron-add` being the other two).
//!
//! Read actions return the same shapes `cron ... --json` already builds
//! (`super::handlers`'s json helpers); write actions return a short summary of
//! what changed. `dsh-builtin`'s side of this tool has already asked the user
//! (or, under a task, already turned a write action into `InputRequired`)
//! before any of this runs.

use anyhow::{Context as _, Result};
use dsh_builtin::config_paths;
use dsh_builtin::shell_capabilities::{CronStore, CronToolHost};
use dsh_types::cron::job::RunQuery;
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
            CronToolAction::Incidents => incidents(&store),
            CronToolAction::Status => status(&store),
            CronToolAction::Doctor => doctor(self, &store),
            CronToolAction::Create => create(&store, request),
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

/// `--read`/`--write`/`--allow-command`/`--allow-mcp`/`--network`/`--env`/
/// `--sandbox`, in the order `parse_add`/`parse_edit` accept them. Shared
/// between `create` and `update` so the two never drift apart on which grant
/// fields exist.
fn push_grant_args(argv: &mut Vec<String>, request: &CronToolRequest) {
    for path in &request.read {
        argv.push("--read".to_string());
        argv.push(path.clone());
    }
    for path in &request.write {
        argv.push("--write".to_string());
        argv.push(path.clone());
    }
    for command in &request.allow_command {
        argv.push("--allow-command".to_string());
        argv.push(command.clone());
    }
    for entry in &request.allow_mcp {
        argv.push("--allow-mcp".to_string());
        argv.push(entry.clone());
    }
    for host in &request.network {
        argv.push("--network".to_string());
        argv.push(host.clone());
    }
    for name in &request.env {
        argv.push("--env".to_string());
        argv.push(name.clone());
    }
    if request.sandbox {
        argv.push("--sandbox".to_string());
    }
}

fn create(store: &SqliteCronStore, request: &CronToolRequest) -> Result<serde_json::Value> {
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
    // A job the agent creates always starts paused, independent of anything
    // in `request` - see the module doc on the `dsh-builtin` side. A person
    // resumes it after checking a first `cron run --now`.
    argv.push("--paused".to_string());
    if request.agent {
        argv.push("--agent".to_string());
        if let Some(tokens) = &request.tokens {
            argv.push("--tokens".to_string());
            argv.push(tokens.clone());
        }
        if let Some(ceiling) = &request.max_tokens_per_day {
            argv.push("--max-tokens-per-day".to_string());
            argv.push(ceiling.clone());
        }
        for criterion in &request.check {
            argv.push("--check".to_string());
            argv.push(criterion.clone());
        }
        push_grant_args(&mut argv, request);
    }

    let schedule = request.schedule.clone().context("`schedule` is required")?;
    argv.push(schedule);
    argv.push("--".to_string());
    argv.push(if request.agent {
        request
            .goal
            .clone()
            .context("`goal` is required for an agent job")?
    } else {
        request
            .command
            .clone()
            .context("`command` is required for a shell job")?
    });

    let parsed = parse_add(&argv).map_err(anyhow::Error::msg)?;
    let force = parsed.force;
    let cwd = current_dir_string()?;
    let spec = build_spec(parsed, cwd).map_err(anyhow::Error::msg)?;
    let name = spec.name.clone();
    let id = store.create(&spec, &std::env::vars().collect(), now(), force)?;

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
    // A partial grant edit has to start from what the job already has -
    // `parse_edit` merges onto it - or `update` with only `--check` would
    // silently drop every existing `--read`/`--write`/`--allow-command`. See
    // `handlers::existing_agent_for_edit`'s doc comment for why a real store
    // error here must propagate rather than being read as "no agent spec".
    let existing_agent = super::handlers::existing_agent_for_edit(store, &selector)?;

    let mut argv: Vec<String> = vec![selector];
    if let Some(name) = &request.name {
        argv.push("--name".to_string());
        argv.push(name.clone());
    }
    if let Some(schedule) = &request.schedule {
        argv.push("--schedule".to_string());
        argv.push(schedule.clone());
    }
    if let Some(goal) = &request.goal {
        argv.push("--goal".to_string());
        argv.push(goal.clone());
    } else if let Some(command) = &request.command {
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
    if let Some(tokens) = &request.tokens {
        argv.push("--tokens".to_string());
        argv.push(tokens.clone());
    }
    if let Some(ceiling) = &request.max_tokens_per_day {
        argv.push("--max-tokens-per-day".to_string());
        argv.push(ceiling.clone());
    }
    for criterion in &request.check {
        argv.push("--check".to_string());
        argv.push(criterion.clone());
    }
    push_grant_args(&mut argv, request);

    let (name, patch) = parse_edit(&argv, existing_agent.as_ref()).map_err(anyhow::Error::msg)?;
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
