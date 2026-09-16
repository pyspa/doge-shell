//! Per-stage completion latency instrumentation, enabled by `DOGESH_COMPLETION_TIMING`.
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tracing::debug;

pub(super) static COMPLETION_STAGE_TIMING_ENABLED: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("DOGESH_COMPLETION_TIMING")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
});

pub(super) struct CompletionTiming {
    enabled: bool,
    last: Instant,
    stages: Vec<(&'static str, Duration)>,
}

impl CompletionTiming {
    pub(super) fn start() -> Self {
        let now = Instant::now();
        Self {
            enabled: *COMPLETION_STAGE_TIMING_ENABLED,
            last: now,
            stages: Vec::new(),
        }
    }

    pub(super) fn mark(&mut self, stage: &'static str) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        self.stages.push((stage, now.duration_since(self.last)));
        self.last = now;
    }

    pub(super) fn finish(mut self, input: &str, outcome: &'static str) {
        if !self.enabled {
            return;
        }
        self.mark(outcome);
        let summary = self
            .stages
            .into_iter()
            .map(|(stage, elapsed)| format!("{stage}={}us", elapsed.as_micros()))
            .collect::<Vec<_>>()
            .join(" ");
        debug!("completion timing input={input:?} {summary}");
    }
}
