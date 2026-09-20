//! Generated lifecycle transition sequences (Layer 2).
//!
//! The deterministic truth tables in `lifecycle_tests` / `job_process_tests`
//! stay as-is; this module adds a reference model plus proptest-generated
//! transition sequences over it. After every transition the SUT (a real
//! `Job` pipeline with synthetic pids) must equal the model stage-for-stage,
//! and the `Job` summary derivation must match.
//!
//! Deliberately OS-free: no `waitpid`, `ECHILD`, signals delivery, or real
//! children here. Those belong to deterministic tests and Layer 3
//! (`resource_contract`). The model only mirrors the in-tree states.
//!
//! If proptest finds a failure it persists the seed under
//! `dsh/proptest-regressions/`; promote the minimized sequence to a
//! deterministic test in `lifecycle_tests.rs` instead of relying on the
//! property test forever.

use super::super::Job;
use super::super::job_process::JobProcess;
use super::super::process::Process;
use super::super::state::ProcessState;
use nix::sys::signal::Signal;
use nix::unistd::Pid;
use proptest::prelude::*;

/// Synthetic pid base: stage `i` owns `10001 + i`. Never a real child.
const SYNTHETIC_PID_BASE: i32 = 10_001;
/// A pid that never belongs to the tree.
const UNKNOWN_PID: i32 = 999_999;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopSig {
    Tstp,
    Stop,
    Ttin,
}

impl StopSig {
    fn signal(self) -> Signal {
        match self {
            StopSig::Tstp => Signal::SIGTSTP,
            StopSig::Stop => Signal::SIGSTOP,
            StopSig::Ttin => Signal::SIGTTIN,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelStage {
    Running,
    Stopped(StopSig),
    Completed(u8),
}

impl ModelStage {
    fn to_state(self, pid: Pid) -> ProcessState {
        match self {
            ModelStage::Running => ProcessState::Running,
            ModelStage::Stopped(sig) => ProcessState::Stopped(pid, sig.signal()),
            ModelStage::Completed(code) => ProcessState::Completed(code, None),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Transition {
    Complete { stage: usize, status: u8 },
    Stop { stage: usize, signal: StopSig },
    Continue { stage: usize },
    MarkStoppedRunning,
    ObserveUnknownPid,
}

struct LifecycleModel {
    stages: Vec<ModelStage>,
}

impl LifecycleModel {
    /// Apply a transition; `false` means the model precondition rejected it
    /// (completed stages are terminal) and both sides must stay unchanged.
    fn apply(&mut self, transition: &Transition) -> bool {
        match *transition {
            Transition::Complete { stage, status } => {
                let Some(slot) = self.stages.get_mut(stage) else {
                    return false;
                };
                if matches!(slot, ModelStage::Completed(_)) {
                    return false;
                }
                *slot = ModelStage::Completed(status);
                true
            }
            Transition::Stop { stage, signal } => {
                let Some(slot) = self.stages.get_mut(stage) else {
                    return false;
                };
                if !matches!(slot, ModelStage::Running) {
                    return false;
                }
                *slot = ModelStage::Stopped(signal);
                true
            }
            Transition::Continue { stage } => {
                let Some(slot) = self.stages.get_mut(stage) else {
                    return false;
                };
                if !matches!(slot, ModelStage::Stopped(_)) {
                    return false;
                }
                *slot = ModelStage::Running;
                true
            }
            Transition::MarkStoppedRunning => {
                for slot in &mut self.stages {
                    if matches!(slot, ModelStage::Stopped(_)) {
                        *slot = ModelStage::Running;
                    }
                }
                true
            }
            Transition::ObserveUnknownPid => true,
        }
    }

    fn expected_summary(&self) -> ProcessState {
        if self
            .stages
            .iter()
            .all(|stage| matches!(stage, ModelStage::Completed(_)))
        {
            let code = match self.stages.last() {
                Some(ModelStage::Completed(code)) => *code,
                _ => 0,
            };
            return ProcessState::Completed(code, None);
        }
        let any_stopped = self
            .stages
            .iter()
            .any(|stage| matches!(stage, ModelStage::Stopped(_)));
        let any_running = self
            .stages
            .iter()
            .any(|stage| matches!(stage, ModelStage::Running));
        if any_stopped && !any_running {
            let (index, sig) = self
                .stages
                .iter()
                .enumerate()
                .find_map(|(index, stage)| match stage {
                    ModelStage::Stopped(sig) => Some((index, *sig)),
                    _ => None,
                })
                .expect("a stopped stage exists");
            return ProcessState::Stopped(
                Pid::from_raw(SYNTHETIC_PID_BASE + index as i32),
                sig.signal(),
            );
        }
        ProcessState::Running
    }
}

fn synth_pid(stage: usize) -> Pid {
    Pid::from_raw(SYNTHETIC_PID_BASE + stage as i32)
}

fn build_sut(stages: usize) -> Job {
    let mut job = Job::new("property".to_string(), Pid::from_raw(1));
    for index in 0..stages {
        let mut process = Process::new(format!("stage-{index}"), vec![]);
        process.pid = Some(synth_pid(index));
        job.set_process(JobProcess::Command(process));
    }
    job
}

fn sut_states(job: &Job) -> Vec<ProcessState> {
    let mut states = Vec::new();
    let mut current = job.process.as_deref();
    while let Some(process) = current {
        states.push(process.get_state());
        current = process.next_process();
    }
    states
}

fn model_states(model: &LifecycleModel) -> Vec<ProcessState> {
    model
        .stages
        .iter()
        .enumerate()
        .map(|(index, stage)| stage.to_state(synth_pid(index)))
        .collect()
}

fn apply_to_sut(job: &mut Job, transition: &Transition) {
    let Some(root) = job.process.as_deref_mut() else {
        return;
    };
    match *transition {
        Transition::Complete { stage, status } => {
            root.set_state_pid(synth_pid(stage), ProcessState::Completed(status, None));
        }
        Transition::Stop { stage, signal } => {
            let pid = synth_pid(stage);
            root.set_state_pid(pid, ProcessState::Stopped(pid, signal.signal()));
        }
        Transition::Continue { stage } => {
            root.set_state_pid(synth_pid(stage), ProcessState::Running);
        }
        Transition::MarkStoppedRunning => {
            root.mark_stopped_processes_running();
        }
        Transition::ObserveUnknownPid => {
            root.set_state_pid(Pid::from_raw(UNKNOWN_PID), ProcessState::Completed(0, None));
        }
    }
}

fn check_invariants(job: &mut Job, model: &LifecycleModel, context: &str) {
    let expected = model_states(model);
    let actual = sut_states(job);
    assert_eq!(
        actual,
        expected,
        "tree diverged from model {context} model stages: {stages:?}",
        stages = model.stages,
    );

    let all_completed = model
        .stages
        .iter()
        .all(|stage| matches!(stage, ModelStage::Completed(_)));
    assert_eq!(
        job.is_process_tree_completed(),
        all_completed,
        "tree completion mismatch {context}"
    );
    // Non-zero exits still count as completed: codes never affect the tree.
    assert_eq!(
        job.process.as_deref().is_none_or(JobProcess::is_completed),
        all_completed,
        "JobProcess::is_completed mismatch {context}"
    );

    let any_stopped = model
        .stages
        .iter()
        .any(|stage| matches!(stage, ModelStage::Stopped(_)));
    assert_eq!(
        job.has_stopped_process(),
        any_stopped,
        "has_stopped_process mismatch {context}"
    );
    let any_running = model
        .stages
        .iter()
        .any(|stage| matches!(stage, ModelStage::Running));
    assert_eq!(
        job.is_fully_stopped(),
        any_stopped && !any_running,
        "is_fully_stopped mismatch {context}"
    );

    job.refresh_lifecycle_state();
    assert_eq!(
        job.state,
        model.expected_summary(),
        "lifecycle summary mismatch {context}"
    );
}

fn stop_sig_strategy() -> impl Strategy<Value = StopSig> {
    prop_oneof![
        Just(StopSig::Tstp),
        Just(StopSig::Stop),
        Just(StopSig::Ttin),
    ]
}

fn transition_strategy() -> impl Strategy<Value = Transition> {
    prop_oneof![
        (0..4usize, any::<u8>()).prop_map(|(stage, status)| Transition::Complete { stage, status }),
        (0..4usize, stop_sig_strategy())
            .prop_map(|(stage, signal)| Transition::Stop { stage, signal }),
        (0..4usize).prop_map(|stage| Transition::Continue { stage }),
        Just(Transition::MarkStoppedRunning),
        Just(Transition::ObserveUnknownPid),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    /// Generated transition sequences preserve every lifecycle invariant.
    /// Rejected (precondition-failing) transitions must change nothing;
    /// accepted ones must keep the SUT equal to the model.
    #[test]
    fn lifecycle_property_preserves_invariants(
        stage_count in 1..=4usize,
        transitions in prop::collection::vec(transition_strategy(), 1..=40),
    ) {
        let mut model = LifecycleModel {
            stages: vec![ModelStage::Running; stage_count],
        };
        let mut sut = build_sut(stage_count);
        check_invariants(&mut sut, &model, "initial");

        for (index, transition) in transitions.iter().enumerate() {
            let context = format!("step {index} ({transition:?})");
            let before = sut_states(&sut);
            if !model.apply(transition) {
                // Terminal stages stay terminal: a rejected transition is a
                // no-op on both sides.
                prop_assert_eq!(sut_states(&sut), before, "rejected transition mutated SUT {}", context);
                check_invariants(&mut sut, &model, &format!("{context} (rejected)"));
                continue;
            }
            if matches!(transition, Transition::ObserveUnknownPid) {
                apply_to_sut(&mut sut, transition);
                prop_assert_eq!(sut_states(&sut), before, "unknown pid mutated the tree {}", context);
            } else {
                apply_to_sut(&mut sut, transition);
            }
            check_invariants(&mut sut, &model, &context);
        }
    }
}

#[test]
fn lifecycle_property_empty_job_summary_is_completed() {
    // `Job::new` without a tree: refresh must not invent a stop.
    let mut job = Job::new("empty".to_string(), Pid::from_raw(1));
    job.refresh_lifecycle_state();
    assert_eq!(job.state, ProcessState::Completed(0, None));
    assert!(!job.is_fully_stopped());
    assert!(!job.has_stopped_process());
}
