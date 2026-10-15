//! A shell-owned task, isolated chat worker and parent-owned tool supervisor.
//! No daemon, persistent queue, extra model loop or implicit permission grant.
mod operation;
pub(crate) mod protocol;
pub(crate) mod supervisor;
mod worker;
use crate::{environment::child_snapshot::ChildShellSnapshot, shell::Shell};
use anyhow::{Result, bail};
use dsh_types::{
    Context,
    background_task::{BudgetLimits, Profile, TaskScope},
};
use operation::{normalize_operation, revalidate_operation};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    os::{fd::AsRawFd, unix::net::UnixStream},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct BackgroundWorkerRequest {
    pub task_id: String,
    pub prompt: String,
    pub scope: TaskScope,
    pub conversation: Option<serde_json::Value>,
}
impl std::fmt::Debug for BackgroundWorkerRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundWorkerRequest")
            .field("task_id", &self.task_id)
            .finish_non_exhaustive()
    }
}
struct WorkerProcess {
    pub pid: nix::unistd::Pid,
    pub channel: protocol::Channel,
    pub shutdown: UnixStream,
    pub stdout: File,
    pub stderr: File,
}

fn filtered_snapshot(shell: &Shell, env_names: &[String]) -> ChildShellSnapshot {
    let mut snapshot = ChildShellSnapshot::capture(&shell.environment.read());
    snapshot.variables.retain(|key, _| {
        key.starts_with("AI_CHAT_")
            || matches!(
                key.as_str(),
                "CHAT_PROMPT"
                    | "AI_MESSAGE_LANG"
                    | "OPENAI_BASE_URL"
                    | "OPENAI_MODEL"
                    | "OPENAI_API_KEY"
                    | "OPEN_AI_API_KEY"
                    | "OPENCODE_GO_API_KEY"
                    | "OPENCODE_ZEN_API_KEY"
                    | "OPENCODE_API_KEY"
                    | "DOGESH_AI_HOOKS_CONFIG"
                    | "XDG_CONFIG_HOME"
                    | "XDG_STATE_HOME"
                    | "XDG_CACHE_HOME"
                    | "XDG_DATA_HOME"
            )
            || env_names.contains(key)
    });
    snapshot.exported_vars.retain(|key| env_names.contains(key));
    snapshot.aliases.clear();
    snapshot.abbreviations.clear();
    snapshot.command_abbreviations.clear();
    snapshot.execute_allowlist.clear();
    snapshot.shell_always_allowlist.clear();
    snapshot.safety_level = "strict".into();
    for (key, value) in [
        ("AI_CHAT_STREAM", "0"),
        ("AI_CHAT_SKILL_REFLECT", "0"),
        ("AI_CHAT_SKILL_AUTO_ARCHIVE_DAYS", "0"),
        ("AI_CHAT_EXECUTE_ALLOWLIST", ""),
    ] {
        snapshot.variables.insert(key.into(), value.into());
    }
    snapshot
}

fn spawn(snapshot: ChildShellSnapshot, request: BackgroundWorkerRequest) -> Result<WorkerProcess> {
    use crate::process::reexec::{
        HelperSpawnSignalPolicy, InternalExecKind, InternalExecRequest, PROTOCOL_VERSION,
        spawn_internal_helper,
    };
    let bytes = serde_json::to_vec(&InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot,
        kind: InternalExecKind::ChatTask(Box::new(request)),
    })?;
    let (parent, child) = UnixStream::pair()?;
    let shutdown = parent.try_clone()?;
    let (out_read, out_write) = crate::process::io::cloexec_pipe()?;
    let (err_read, err_write) = crate::process::io::cloexec_pipe()?;
    let null = File::open("/dev/null")?;
    let channel = protocol::Channel::new(parent)?;
    let pid = spawn_internal_helper(
        null.as_raw_fd(),
        out_write.as_raw_fd(),
        err_write.as_raw_fd(),
        &bytes,
        nix::unistd::Pid::from_raw(0),
        Some(child.as_raw_fd()),
        HelperSpawnSignalPolicy::Normal,
    )?;
    Ok(WorkerProcess {
        pid,
        channel,
        shutdown,
        stdout: File::from(out_read),
        stderr: File::from(err_read),
    })
}

pub(crate) fn run_worker(
    shell: &mut Shell,
    request: &BackgroundWorkerRequest,
    fd: i32,
) -> Result<u8> {
    worker::run(shell, request, fd)
}

pub(crate) fn command(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    if shell.background_context.is_some() {
        bail!("chat_task requires the parent shell");
    }
    match argv.get(1).map(String::as_str).unwrap_or("status") {
        "start" => {
            if shell
                .background_task
                .as_ref()
                .is_some_and(supervisor::TaskHandle::is_active)
            {
                bail!("one background task is already active");
            }
            let mut profile = None;
            let mut env_names = vec![
                "PATH".into(),
                "LANG".into(),
                "LC_ALL".into(),
                "TMPDIR".into(),
            ];
            let mut at = 2;
            while let Some(arg) = argv.get(at) {
                if arg == "--" {
                    at += 1;
                    break;
                }
                match arg.as_str() {
                    "--profile" => {
                        at += 1;
                        profile = Some(match argv.get(at).map(String::as_str) {
                            Some("read-only") => Profile::ReadOnly,
                            Some("workspace-edit") => Profile::WorkspaceEdit,
                            Some("sandboxed-test") => bail!(
                                "sandboxed-test is unavailable until effective OS isolation is verified; no fallback"
                            ),
                            _ => bail!("choose read-only or workspace-edit"),
                        });
                    }
                    "--env" => {
                        at += 1;
                        env_names.push(
                            argv.get(at)
                                .ok_or_else(|| anyhow::anyhow!("--env needs a name"))?
                                .clone(),
                        );
                    }
                    _ => bail!(
                        "usage: chat_task start --profile <read-only|workspace-edit> [--env NAME] -- <prompt>"
                    ),
                }
                at += 1;
            }
            let profile = profile.ok_or_else(|| anyhow::anyhow!("explicit --profile required"))?;
            let prompt = argv[at..].join(" ");
            if prompt.trim().is_empty() {
                bail!("task prompt required");
            }
            dsh_builtin::validate_background_configuration(shell).map_err(anyhow::Error::msg)?;
            let mut snapshot = filtered_snapshot(shell, &env_names);
            snapshot.cwd = snapshot.cwd.canonicalize()?;
            let scope = TaskScope {
                profile,
                cwd: snapshot.cwd.clone(),
                read_roots: vec![snapshot.cwd.clone()],
                write_roots: if profile == Profile::WorkspaceEdit {
                    vec![snapshot.cwd.clone()]
                } else {
                    Vec::new()
                },
                environment_names: env_names,
                network_hosts: Vec::new(),
                sandbox_verified: false,
                budget: BudgetLimits {
                    time_ms: 600_000,
                    tool_calls: 100,
                    tokens: 64_000,
                    output_bytes: 8 * 1024 * 1024,
                },
            };
            scope.validate().map_err(anyhow::Error::msg)?;
            snapshot.variables.insert(
                "AI_CHAT_TURN_TOKEN_BUDGET".into(),
                scope.budget.tokens.to_string(),
            );
            let task_id = format!("chat-{}", uuid::Uuid::new_v4());
            let request = BackgroundWorkerRequest {
                task_id: task_id.clone(),
                prompt,
                scope: scope.clone(),
                conversation: dsh_builtin::background_session_snapshot(),
            };
            let worker = spawn(snapshot.clone(), request)?;
            shell.background_task = Some(supervisor::TaskHandle::launch(
                task_id.clone(),
                snapshot,
                scope,
                worker,
            )?);
            ctx.write_stdout(&format!(
                "{task_id} started; chat_task status/output/cancel\n"
            ))?;
        }
        "status" => ctx.write_stdout(&match &shell.background_task {
            Some(task) => serde_json::to_string_pretty(&task.status())?,
            None => "no background task".into(),
        })?,
        "output" => ctx.write_stdout(
            &shell
                .background_task
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no background task"))?
                .output(),
        )?,
        "cancel" => {
            shell
                .background_task
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("no background task"))?
                .cancel();
            ctx.write_stdout("task cancelled\n")?;
        }
        "approve" | "deny" => {
            let expected_task = argv
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("task id required"))?;
            let id = argv
                .get(3)
                .ok_or_else(|| anyhow::anyhow!("approval request id required"))?
                .parse()?;
            shell
                .background_task
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no background task"))?
                .approve(expected_task, id, argv[1] == "approve")?;
            ctx.write_stdout("approval response recorded\n")?;
        }
        "restrict" if argv.get(2).is_some_and(|s| s == "read-only") => {
            shell
                .background_task
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no background task"))?
                .restrict_read_only()?;
            ctx.write_stdout("task restricted to read-only\n")?;
        }
        _ => {
            bail!(
                "chat_task: start/status/output/cancel/approve <task-id> <request-id>/deny <task-id> <request-id>/restrict read-only"
            )
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opencode_credentials_are_available_to_ai_without_expanding_execute_exports() {
        let env = crate::environment::Environment::new();
        for key in [
            "OPENCODE_GO_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "OPENCODE_API_KEY",
        ] {
            env.write().set_shell_var(key.into(), "fake-key".into());
        }
        let shell = Shell::new(env);
        let snapshot = filtered_snapshot(&shell, &[]);
        for key in [
            "OPENCODE_GO_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "OPENCODE_API_KEY",
        ] {
            assert_eq!(
                snapshot.variables.get(key).map(String::as_str),
                Some("fake-key")
            );
            assert!(!snapshot.exported_vars.contains(key));
        }
    }
}
