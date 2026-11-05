//! Read-only, bounded observations of supervisor authority for the inline REPL.
//! Already answered approvals are obsolete; notices never carry operation data.
use super::*;
use std::collections::VecDeque;

const RETIRED_LIMIT: usize = 32;

#[derive(Default)]
pub(super) struct Notices {
    approval: Option<(u64, u64)>,
    terminal: bool,
    retired: VecDeque<String>,
}

impl TaskHandle {
    /// Consume presentation records only. Never expire or answer an approval.
    pub(crate) fn take_notices(&self) -> Vec<String> {
        let mut inner = self.shared.inner.lock();
        let mut lines: Vec<_> = inner.notices.retired.drain(..).collect();
        let pending = inner.pending.as_ref().filter(|pending| {
            pending.decision.is_none()
                && inner.task.status() == TaskStatus::InputRequired
                && inner.task.pending() == Some(&pending.request)
                && !self.shared.canceled.load(Ordering::Acquire)
        });
        let approval = pending.map(|pending| (pending.request.request_id, pending.request.epoch));
        let state = match inner.task.status() {
            TaskStatus::Completed if inner.completed_ok == Some(true) => Some("completed"),
            TaskStatus::Completed if inner.completed_ok == Some(false) => Some("failed"),
            TaskStatus::Cancelled if inner.completed_ok == Some(false) && !inner.user_cancelled => {
                Some("failed")
            }
            TaskStatus::TimedOut => Some("failed (timed_out)"),
            TaskStatus::BudgetExhausted => Some("failed (budget_exhausted)"),
            _ => None,
        };
        // IDs are host-generated UUIDs. Escaping also protects callers using
        // TaskState directly, without ever borrowing worker-controlled text.
        let id = safe_output(inner.task.task_id(), 80);
        if approval.is_some() && inner.notices.approval != approval {
            lines.push(format!("[AI task {id}] approval_waiting"));
            inner.notices.approval = approval;
        }
        if let Some(state) = state.filter(|_| !inner.notices.terminal) {
            lines.push(format!("[AI task {id}] {state}"));
            inner.notices.terminal = true;
        }
        lines
    }

    /// Called only after replacement launch succeeds. Retain terminal notices
    /// that completed before the next REPL tick, without retaining old tasks.
    pub(in crate::background_task) fn inherit_notices(&self, previous: &TaskHandle) {
        let lines = previous.take_notices();
        let mut inner = self.shared.inner.lock();
        for line in lines {
            if inner.notices.retired.len() == RETIRED_LIMIT {
                inner.notices.retired.pop_front();
            }
            inner.notices.retired.push_back(line);
        }
    }
}
