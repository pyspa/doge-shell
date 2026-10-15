//! Atomic approval displays and one-time answers. A stale UI never mutates a
//! different pending request; effect-time consumption remains in the supervisor.
use super::*;

pub(in crate::background_task) struct ApprovalPreview {
    pub display: Value,
    pub complete: bool,
    request: ApprovalRequest,
    guidance: bool,
}

pub(super) fn display(shared: &Shared, inner: &Inner, pending: &Pending) -> (Value, bool) {
    let operation = &pending.request.operation;
    let mut redacted = false;
    let mut truncated = false;
    let mut preview = |text: &str, limit| {
        let masked = shared.redact(text);
        redacted |= masked.redacted;
        let full = super::output::escape_display(&masked.text, false);
        truncated |= full.len() > limit;
        full[..full.floor_char_boundary(limit)].to_string()
    };
    let targets: Vec<_> = operation
        .canonical_targets
        .iter()
        .map(|path| preview(&path.to_string_lossy(), 512))
        .collect();
    let guidance = if pending.guidance {
        operation.args["guidance_trust"]
            .as_str()
            .map(|text| preview(text, 2048))
    } else {
        None
    };
    let command = operation.args["command"]
        .as_str()
        .map(|text| preview(text, 2048));
    let before = operation
        .content_digest
        .as_ref()
        .and(operation.preimage_digest.as_ref())
        .map(|text| preview(text, 2048));
    let after = operation
        .content_digest
        .as_ref()
        .map(|text| preview(text, 2048));
    let complete = !redacted
        && !truncated
        && operation
            .canonical_targets
            .iter()
            .all(|path| path.to_str().is_some());
    (
        json!({
            "task_id": pending.request.task_id,
            "request_id": pending.request.request_id, "epoch": pending.request.epoch,
            "tool": if pending.guidance { "project_guidance" } else { &operation.tool },
            "targets": targets, "target_count": operation.canonical_targets.len(),
            "guidance": guidance, "command_preview": command,
            "execution_warning": if operation.tool == "execute" && !inner.task.scope().sandbox_verified { Some("This command will run without OS sandbox isolation.") } else { None },
            "write_bytes": operation.content_digest.as_ref().map(String::len),
            "before_preview": before, "after_preview": after, "preview_truncated": truncated, "preview_redacted": redacted, "preview_complete": complete,
        }),
        complete,
    )
}

fn matches(inner: &Inner, preview: &ApprovalPreview) -> bool {
    active(inner.task.status())
        && inner.task.task_id() == preview.request.task_id
        && inner.task.generation() == preview.request.epoch
        && inner.task.pending() == Some(&preview.request)
        && inner.pending.as_ref().is_some_and(|pending| {
            pending.request == preview.request
                && pending.guidance == preview.guidance
                && pending.decision.is_none()
        })
}

impl TaskHandle {
    pub(in crate::background_task) fn approval_preview(&self) -> Option<ApprovalPreview> {
        self.shared.cancel_disconnected();
        let mut inner = self.shared.inner.lock();
        inner.task.check_deadline(self.shared.elapsed());
        let pending = inner.pending.as_ref()?;
        if !active(inner.task.status())
            || inner.task.pending() != Some(&pending.request)
            || pending.decision.is_some()
        {
            return None;
        }
        let (display, complete) = display(&self.shared, &inner, pending);
        Some(ApprovalPreview {
            display,
            complete,
            request: pending.request.clone(),
            guidance: pending.guidance,
        })
    }

    pub(in crate::background_task) fn respond_preview(
        &self,
        preview: &ApprovalPreview,
        allow: bool,
    ) -> Result<()> {
        self.shared.cancel_disconnected();
        let scope = {
            let mut inner = self.shared.inner.lock();
            inner.task.check_deadline(self.shared.elapsed());
            if self.shared.canceled.load(Ordering::Acquire) || !matches(&inner, preview) {
                bail!("background approval is stale or unknown");
            }
            if allow && !preview.complete {
                bail!("incomplete preview: approval unavailable");
            }
            inner.task.scope().clone()
        };
        let current = if !allow || preview.guidance {
            Ok(preview.request.operation.clone())
        } else {
            super::super::revalidate_operation(&preview.request.operation, &scope)
        };
        self.shared.cancel_disconnected();
        let mut inner = self.shared.inner.lock();
        inner.task.check_deadline(self.shared.elapsed());
        // The comparison and decision mutation share this lock. Never delegate
        // through a separately locked ID-only decision path.
        if self.shared.canceled.load(Ordering::Acquire) || !matches(&inner, preview) {
            bail!("background approval is stale or unknown");
        }
        if current.as_ref().ok() != Some(&preview.request.operation) {
            inner.task.deny_approval();
            inner.pending = None;
            self.shared.changed.notify_all();
            bail!("background approval expired: operation changed");
        }
        inner.pending.as_mut().expect("matched pending").decision = Some(allow);
        self.shared.changed.notify_all();
        Ok(())
    }
    pub(in crate::background_task) fn approve(
        &self,
        expected_task_id: &str,
        request_id: u64,
        allow: bool,
    ) -> Result<()> {
        let preview = self
            .approval_preview()
            .ok_or_else(|| anyhow!("background approval is stale or unknown"))?;
        if preview.request.task_id != expected_task_id || preview.request.request_id != request_id {
            bail!("background approval is stale or unknown");
        }
        self.respond_preview(&preview, allow)
    }
}
