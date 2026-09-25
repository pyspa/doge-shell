//! Generation/epoch ownership for asynchronous prompt probes.
//!
//! A probe may outlive the runtime snapshot that launched it. This module
//! prevents stale success/failure publication and suppresses duplicate
//! same-runtime probes without cancelling old Tokio tasks.

use super::runtime::PromptRuntimeIdentity;
use std::collections::HashMap;

/// Monotonically advancing owner of a prompt runtime. Every identity change
/// bumps the epoch — even A → B → A — so a stale task holding an old epoch
/// can never publish over the current runtime (ABA protection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PromptProbeEpoch(u64);

/// Async probes managed by the lifecycle. AWS is a synchronous pure lookup
/// and is intentionally excluded; its cache is still invalidated on
/// identity changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PromptProbe {
    Rust,
    Node,
    Python,
    Go,
    Kubernetes,
    Docker,
    /// Git root + status refreshes as one unit: the task holds a single
    /// runtime snapshot across both lookups and publishes under this probe's
    /// epoch, so a stale task cannot overwrite a newer runtime's root or
    /// status.
    Git,
}

/// Result of observing a runtime identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObserveResult {
    pub epoch: PromptProbeEpoch,
    pub changed: bool,
}

/// Tracks which runtime is current and which probes are in flight for it.
///
/// Knows only runtime identity, epoch, probe kind, and in-flight state —
/// never version strings, namespaces, backoff durations, or project types.
#[derive(Debug)]
pub(crate) struct PromptProbeLifecycle {
    identity: Option<PromptRuntimeIdentity>,
    epoch: PromptProbeEpoch,
    inflight: HashMap<PromptProbe, PromptProbeEpoch>,
}

impl Default for PromptProbeLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptProbeLifecycle {
    pub(crate) fn new() -> Self {
        Self {
            identity: None,
            epoch: PromptProbeEpoch(0),
            inflight: HashMap::new(),
        }
    }

    /// Observe a runtime identity. Same identity keeps epoch, caches, and
    /// in-flight state; a changed identity bumps the epoch, replaces the
    /// stored identity, and drops in-flight entries (old tasks keep
    /// running but their completions are discarded by the epoch guard).
    pub(crate) fn observe(&mut self, identity: PromptRuntimeIdentity) -> ObserveResult {
        if self.identity.as_ref() == Some(&identity) {
            return ObserveResult {
                epoch: self.epoch,
                changed: false,
            };
        }
        self.epoch = PromptProbeEpoch(self.epoch.0.wrapping_add(1));
        self.identity = Some(identity);
        self.inflight.clear();
        ObserveResult {
            epoch: self.epoch,
            changed: true,
        }
    }

    /// Claim the in-flight slot for `probe` at `epoch`. Stale epochs and
    /// duplicate same-epoch claims return false.
    pub(crate) fn try_begin(&mut self, probe: PromptProbe, epoch: PromptProbeEpoch) -> bool {
        if epoch != self.epoch {
            return false;
        }
        if self.inflight.get(&probe) == Some(&epoch) {
            return false;
        }
        self.inflight.insert(probe, epoch);
        true
    }

    /// Release a probe slot and report whether the caller may publish.
    ///
    /// Only the slot owner releases its entry: a stale completion must
    /// never clear a newer epoch's in-flight slot. Publication additionally
    /// requires the epoch to still be current.
    pub(crate) fn finish(&mut self, probe: PromptProbe, epoch: PromptProbeEpoch) -> bool {
        let owns_slot = self.inflight.get(&probe) == Some(&epoch);
        if owns_slot {
            self.inflight.remove(&probe);
        }
        owns_slot && self.epoch == epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::runtime::PromptEnvironment;
    use std::path::Path;
    use std::sync::Arc;

    fn identity_for(
        path_generation: u64,
        cwd: &Path,
        child_env: &[(&str, &str)],
        prompt_env: PromptEnvironment,
    ) -> PromptRuntimeIdentity {
        PromptRuntimeIdentity {
            path_generation,
            current_dir: cwd.to_path_buf(),
            child_env: Arc::new(
                child_env
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            environment: prompt_env,
        }
    }

    fn base_identity() -> PromptRuntimeIdentity {
        identity_for(
            10,
            Path::new("/project-a"),
            &[],
            PromptEnvironment::default(),
        )
    }

    #[test]
    fn same_identity_keeps_epoch() {
        let mut lifecycle = PromptProbeLifecycle::new();
        let first = lifecycle.observe(base_identity());
        let second = lifecycle.observe(base_identity());
        assert!(!second.changed);
        assert_eq!(first.epoch, second.epoch);
    }

    #[test]
    fn identity_equality_ignores_hashmap_insertion_order() {
        let left = identity_for(
            10,
            Path::new("/project-a"),
            &[("A", "1"), ("B", "2")],
            PromptEnvironment::default(),
        );
        let right = identity_for(
            10,
            Path::new("/project-a"),
            &[("B", "2"), ("A", "1")],
            PromptEnvironment::default(),
        );
        assert_eq!(left, right);
    }

    #[test]
    fn generation_change_bumps_epoch() {
        let mut lifecycle = PromptProbeLifecycle::new();
        let first = lifecycle.observe(base_identity());
        let mut changed = base_identity();
        changed.path_generation = 11;
        let second = lifecycle.observe(changed);
        assert!(second.changed);
        assert_ne!(first.epoch, second.epoch);
    }

    #[test]
    fn duplicate_same_epoch_begin_is_rejected() {
        let mut lifecycle = PromptProbeLifecycle::new();
        let observed = lifecycle.observe(base_identity());
        assert!(lifecycle.try_begin(PromptProbe::Node, observed.epoch));
        assert!(!lifecycle.try_begin(PromptProbe::Node, observed.epoch));
    }

    #[test]
    fn new_epoch_can_start_immediately() {
        let mut lifecycle = PromptProbeLifecycle::new();
        let first = lifecycle.observe(base_identity());
        assert!(lifecycle.try_begin(PromptProbe::Node, first.epoch));
        let mut other = base_identity();
        other.path_generation = 11;
        let second = lifecycle.observe(other);
        assert!(second.changed);
        assert!(lifecycle.try_begin(PromptProbe::Node, second.epoch));
    }

    #[test]
    fn epoch_handles_a_b_a() {
        let mut lifecycle = PromptProbeLifecycle::new();
        let epoch_a1 = lifecycle.observe(base_identity()).epoch;
        assert!(lifecycle.try_begin(PromptProbe::Node, epoch_a1));
        let mut identity_b = base_identity();
        identity_b.path_generation = 11;
        let epoch_b = lifecycle.observe(identity_b).epoch;
        assert_ne!(epoch_a1, epoch_b);
        let epoch_a2 = lifecycle.observe(base_identity()).epoch;
        assert_ne!(epoch_a1, epoch_a2);
        assert_ne!(epoch_b, epoch_a2);
        assert!(lifecycle.try_begin(PromptProbe::Node, epoch_a2));
        // Old A completion is stale even though the identity text matches.
        assert!(!lifecycle.finish(PromptProbe::Node, epoch_a1));
        assert!(lifecycle.finish(PromptProbe::Node, epoch_a2));
    }

    #[test]
    fn stale_completion_keeps_current_slot() {
        let mut lifecycle = PromptProbeLifecycle::new();
        let epoch1 = lifecycle.observe(base_identity()).epoch;
        assert!(lifecycle.try_begin(PromptProbe::Node, epoch1));
        let mut other = base_identity();
        other.path_generation = 11;
        let epoch2 = lifecycle.observe(other).epoch;
        assert!(lifecycle.try_begin(PromptProbe::Node, epoch2));
        assert!(!lifecycle.finish(PromptProbe::Node, epoch1));
        // Epoch 2 still owns its slot, so a duplicate begin is rejected.
        assert!(!lifecycle.try_begin(PromptProbe::Node, epoch2));
        assert!(lifecycle.finish(PromptProbe::Node, epoch2));
    }

    #[test]
    fn current_completion_frees_slot() {
        let mut lifecycle = PromptProbeLifecycle::new();
        let epoch = lifecycle.observe(base_identity()).epoch;
        assert!(lifecycle.try_begin(PromptProbe::Node, epoch));
        assert!(lifecycle.finish(PromptProbe::Node, epoch));
        assert!(lifecycle.try_begin(PromptProbe::Node, epoch));
    }

    #[test]
    fn stale_epoch_begin_is_rejected() {
        let mut lifecycle = PromptProbeLifecycle::new();
        let epoch1 = lifecycle.observe(base_identity()).epoch;
        let mut other = base_identity();
        other.path_generation = 11;
        lifecycle.observe(other);
        assert!(!lifecycle.try_begin(PromptProbe::Node, epoch1));
    }
}
