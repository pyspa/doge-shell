//! Dedicated worker transport. The parent owns all tool execution and approval.
use super::{
    BackgroundWorkerRequest,
    protocol::{Channel, ParentMessage, WorkerMessage},
};
use dsh_builtin::shell_capabilities::{ApprovalDecision, BackgroundTaskContext};
use parking_lot::Mutex;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

struct Remote {
    channel: Mutex<Channel>,
    serial: AtomicU64,
    cancelled: AtomicBool,
    cwd: PathBuf,
    deadline: Instant,
}
impl Remote {
    fn remaining(&self) -> anyhow::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow::anyhow!("background deadline reached"))
    }
}
impl BackgroundTaskContext for Remote {
    fn remote_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        // Observations are the worker conversation's own bounded in-memory data.
        if name == "observation_read" {
            return None;
        }
        Some(
            (|| {
                let serial = self.serial.fetch_add(1, Ordering::SeqCst);
                let mut channel = self.channel.lock();
                channel.send(
                    &WorkerMessage::Tool {
                        serial,
                        name: name.into(),
                        arguments: args.into(),
                    },
                    self.remaining()?,
                )?;
                match channel.receive::<ParentMessage>(self.remaining()?)? {
                    ParentMessage::Reply {
                        serial: reply,
                        result,
                    } if reply == serial => Ok(result),
                    _ => anyhow::bail!("background response identity mismatch"),
                }
            })()
            .unwrap_or_else(|_: anyhow::Error| {
                self.cancelled.store(true, Ordering::SeqCst);
                Err("background transport failed or was cancelled".into())
            }),
        )
    }
    fn permits_tool(&self, name: &str, _: &str) -> bool {
        name == "observation_read"
    }
    fn confirm(&self, message: &str) -> anyhow::Result<ApprovalDecision> {
        let serial = self.serial.fetch_add(1, Ordering::SeqCst);
        let mut channel = self.channel.lock();
        channel.send(
            &WorkerMessage::Confirm {
                serial,
                message: message.into(),
            },
            self.remaining()?,
        )?;
        match channel.receive::<ParentMessage>(self.remaining()?)? {
            ParentMessage::Approval {
                serial: reply,
                allow,
            } if reply == serial => Ok(if allow {
                ApprovalDecision::Allow
            } else {
                ApprovalDecision::Deny
            }),
            _ => anyhow::bail!("background approval identity mismatch"),
        }
    }
    fn is_canceled(&self) -> bool {
        if crate::process::signal::check_and_clear_sigint() || Instant::now() >= self.deadline {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        self.cancelled.load(Ordering::SeqCst)
    }
    fn cwd(&self) -> PathBuf {
        self.cwd.clone()
    }
    fn jobs(&self) -> Option<Arc<Mutex<dsh_builtin::agent::jobs::AgentJobs>>> {
        None
    }
    fn read_authorized(&self, _: &Path) -> bool {
        false
    }
    fn read_result_authorized(&self, _: &Path, _: &str) -> bool {
        false
    }
    fn write_authorized(&self, _: &Path) -> bool {
        false
    }
    fn write_contents_authorized(&self, _: &Path, _: &str) -> bool {
        false
    }
}

pub(super) fn run(
    shell: &mut crate::shell::Shell,
    request: &BackgroundWorkerRequest,
    fd: i32,
) -> anyhow::Result<u8> {
    use std::os::fd::FromRawFd;
    // The helper validated this dedicated status descriptor and set CLOEXEC.
    let socket = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    let remote = Arc::new(Remote {
        channel: Mutex::new(Channel::new(socket)?),
        serial: AtomicU64::new(1),
        cancelled: AtomicBool::new(false),
        cwd: request.scope.cwd.clone(),
        deadline: Instant::now() + Duration::from_millis(request.scope.budget.time_ms),
    });
    dsh_builtin::enter_background_session(&request.task_id, request.conversation.clone())
        .map_err(anyhow::Error::msg)?;
    shell.background_context = Some(remote.clone());
    shell.set_signals();
    let mut ctx = dsh_types::Context::new_safe(shell.pid, shell.pgid, false);
    ctx.interactive = false;
    ctx.save_history = false;
    let result = dsh_builtin::execute_chat_message(&ctx, shell, &request.prompt, None);
    let ok = matches!(result, dsh_types::ExitStatus::ExitedWith(0));
    let _ = remote.channel.lock().send(
        &WorkerMessage::Complete {
            ok,
            tokens_used: dsh_openai::usage::session_total().total_tokens(),
        },
        Duration::from_secs(1),
    );
    Ok(if ok { 0 } else { 1 })
}
