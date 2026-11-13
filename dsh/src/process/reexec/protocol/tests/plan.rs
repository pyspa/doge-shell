use super::super::*;
use std::os::fd::FromRawFd as _;

fn plan_request(mode: PlanExecMode, signal_policy: PlanSignalPolicy) -> InternalExecRequest {
    plan_request_for_test(mode, signal_policy)
}

fn roundtrip_plan_policy(
    mode: PlanExecMode,
    signal_policy: PlanSignalPolicy,
) -> (PlanExecMode, PlanSignalPolicy) {
    let request = plan_request(mode, signal_policy);
    let bytes = serde_json::to_vec(&request).expect("encode");
    assert!(bytes.len() <= MAX_INTERNAL_EXEC_REQUEST);
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let decoded = read_internal_request(read.into_raw_fd()).expect("decode");
    match decoded.kind {
        InternalExecKind::Plan(plan) => {
            assert_eq!(decoded.version, PROTOCOL_VERSION);
            (plan.mode, plan.signal_policy)
        }
        other => panic!("expected Plan, got {other:?}"),
    }
}

#[test]
fn plan_signal_policy_roundtrips_through_pipe() {
    // The async pair must survive serialization: the helper commits its
    // signal dispositions from exactly these two fields.
    assert_eq!(
        roundtrip_plan_policy(
            PlanExecMode::AsyncAndOrList,
            PlanSignalPolicy::IgnoreInterruptAndQuit
        ),
        (
            PlanExecMode::AsyncAndOrList,
            PlanSignalPolicy::IgnoreInterruptAndQuit
        )
    );
    assert_eq!(
        roundtrip_plan_policy(PlanExecMode::AsyncAndOrList, PlanSignalPolicy::Normal),
        (PlanExecMode::AsyncAndOrList, PlanSignalPolicy::Normal)
    );
    assert_eq!(
        roundtrip_plan_policy(PlanExecMode::Subshell, PlanSignalPolicy::Normal),
        (PlanExecMode::Subshell, PlanSignalPolicy::Normal)
    );
}

#[test]
fn plan_signal_policy_validation_accepts_known_combinations() {
    use PlanExecMode::*;
    use PlanSignalPolicy::*;
    for mode in [
        AsyncAndOrList,
        Subshell,
        CommandSubstitution,
        ProcessSubstitution,
    ] {
        validate_plan_signal_policy(mode, Normal)
            .unwrap_or_else(|_| panic!("{mode:?} + Normal must be valid"));
    }
    validate_plan_signal_policy(AsyncAndOrList, IgnoreInterruptAndQuit)
        .expect("AsyncAndOrList + IgnoreInterruptAndQuit must be valid");
}

#[test]
fn plan_signal_policy_validation_rejects_non_async_ignore() {
    use PlanExecMode::*;
    use PlanSignalPolicy::*;
    for mode in [Subshell, CommandSubstitution, ProcessSubstitution] {
        let err = validate_plan_signal_policy(mode, IgnoreInterruptAndQuit)
            .expect_err("non-async ignore must fail closed");
        assert!(err.to_string().contains("invalid signal policy"));
    }
}

fn plan_with_substitution_direction(
    direction: crate::shell::plan::ProcessSubstitutionDirection,
) -> crate::shell::plan::ExecutionPlan {
    use crate::shell::plan::{PlannedSubstitution, PlannedSubstitutionKind};

    let env_arc = crate::environment::Environment::new();
    let inner =
        crate::shell::parse::parse_execution_plan("printf x", env_arc).expect("parse inner");
    let substitution = PlannedSubstitution {
        source: "printf x".to_string(),
        kind: PlannedSubstitutionKind::Process(direction),
        plan: Box::new(inner),
    };
    // Minimal outer plan carrying the substitution in argv: serialize the
    // whole `ExecutionPlan` so the direction travels the re-exec JSON.
    // Reuse the real parser for the outer shape, then swap the kind to
    // the requested direction (avoids hand-building jobs).
    let env_arc = crate::environment::Environment::new();
    let mut outer =
        crate::shell::parse::parse_execution_plan("cat <(printf x)", env_arc).expect("parse outer");
    for job in outer.lists.iter_mut().flat_map(|list| list.jobs.iter_mut()) {
        for stage in job.stages.iter_mut() {
            for word in stage.argv.iter_mut() {
                for part in word.parts.iter_mut() {
                    if let crate::shell::plan::WordPart::Substitution {
                        substitution: inner_sub,
                        ..
                    } = part
                    {
                        inner_sub.kind = PlannedSubstitutionKind::Process(direction);
                        inner_sub.source = substitution.source.clone();
                        inner_sub.plan = substitution.plan.clone();
                    }
                }
            }
        }
    }
    outer
}

fn roundtrip_plan_direction(
    direction: crate::shell::plan::ProcessSubstitutionDirection,
) -> crate::shell::plan::ProcessSubstitutionDirection {
    use crate::shell::plan::{PlannedSubstitutionKind, WordPart};

    let env_arc = crate::environment::Environment::new();
    let snapshot = ChildShellSnapshot::capture(&env_arc.read());
    let plan = plan_with_substitution_direction(direction);
    let request = InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot,
        kind: InternalExecKind::Plan(PlanExecRequest {
            plan,
            mode: PlanExecMode::ProcessSubstitution,
            signal_policy: PlanSignalPolicy::Normal,
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    assert!(bytes.len() <= MAX_INTERNAL_EXEC_REQUEST);
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let decoded = read_internal_request(read.into_raw_fd()).expect("decode");
    match decoded.kind {
        InternalExecKind::Plan(plan_request) => {
            assert_eq!(decoded.version, PROTOCOL_VERSION);
            let mut found = None;
            for job in plan_request.plan.iter_jobs() {
                for stage in &job.stages {
                    for word in &stage.argv {
                        for part in &word.parts {
                            if let WordPart::Substitution { substitution, .. } = part
                                && let PlannedSubstitutionKind::Process(dir) = &substitution.kind
                            {
                                found = Some(*dir);
                            }
                        }
                    }
                }
            }
            found.expect("direction must survive round-trip")
        }
        other => panic!("expected Plan, got {other:?}"),
    }
}

#[test]
fn process_substitution_direction_roundtrips_through_v7() {
    use crate::shell::plan::ProcessSubstitutionDirection::{Read, Write};

    assert_eq!(roundtrip_plan_direction(Read), Read);
    assert_eq!(roundtrip_plan_direction(Write), Write);
}

#[test]
fn parameter_expansion_roundtrips_through_v8() {
    use crate::shell::plan::{ParameterAction, ParameterCondition, QuoteMode, WordPart};

    let env_arc = crate::environment::Environment::new();
    let plan = crate::shell::parse::parse_execution_plan("echo ${A:-${B:-fallback}}", env_arc)
        .expect("parse outer");
    // Outer word is `echo` + one parameter expansion.
    let word = &plan.lists[0].jobs[0].stages[0].argv[1];
    let (outer, outer_quote) = match &word.parts[0] {
        WordPart::ParameterExpansion { expansion, quote } => (expansion, *quote),
        other => panic!("expected outer ParameterExpansion, got {other:?}"),
    };
    assert_eq!(outer.name, "A");
    assert_eq!(outer.condition, ParameterCondition::UnsetOrNull);
    assert_eq!(outer.action, ParameterAction::Default);
    assert_eq!(outer_quote, QuoteMode::Unquoted);
    let operand = outer.word.as_deref().expect("outer operand");
    assert_eq!(operand.parts.len(), 1);
    match &operand.parts[0] {
        WordPart::ParameterExpansion { expansion, quote } => {
            assert_eq!(expansion.name, "B");
            assert_eq!(expansion.condition, ParameterCondition::UnsetOrNull);
            assert_eq!(expansion.action, ParameterAction::Default);
            assert_eq!(*quote, QuoteMode::Unquoted);
            let inner_operand = expansion.word.as_deref().expect("inner operand");
            assert_eq!(inner_operand.source, "fallback");
        }
        other => panic!("expected nested ParameterExpansion, got {other:?}"),
    }

    // Serialize the whole plan through the re-exec JSON and verify the
    // structure survives verbatim.
    let snapshot = ChildShellSnapshot::capture(&crate::environment::Environment::new().read());
    let request = InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot,
        kind: InternalExecKind::Plan(PlanExecRequest {
            plan,
            mode: PlanExecMode::CommandSubstitution,
            signal_policy: PlanSignalPolicy::Normal,
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    assert!(bytes.len() <= MAX_INTERNAL_EXEC_REQUEST);
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let decoded = read_internal_request(read.into_raw_fd()).expect("decode");
    assert_eq!(decoded.version, PROTOCOL_VERSION);
    match decoded.kind {
        InternalExecKind::Plan(plan_request) => {
            let word = &plan_request.plan.lists[0].jobs[0].stages[0].argv[1];
            match &word.parts[0] {
                WordPart::ParameterExpansion { expansion, quote } => {
                    assert_eq!(expansion.name, "A");
                    assert_eq!(expansion.condition, ParameterCondition::UnsetOrNull);
                    assert_eq!(expansion.action, ParameterAction::Default);
                    assert_eq!(*quote, QuoteMode::Unquoted);
                    let operand = expansion.word.as_deref().expect("operand");
                    match &operand.parts[0] {
                        WordPart::ParameterExpansion { expansion, quote } => {
                            assert_eq!(expansion.name, "B");
                            assert_eq!(*quote, QuoteMode::Unquoted);
                            assert!(expansion.word.is_some());
                        }
                        other => panic!("nested lost, got {other:?}"),
                    }
                }
                other => panic!("outer lost, got {other:?}"),
            }
        }
        other => panic!("expected Plan, got {other:?}"),
    }
}

#[test]
fn arithmetic_expansion_roundtrips_through_v9() {
    use crate::shell::plan::{QuoteMode, WordPart};

    let env_arc = crate::environment::Environment::new();
    let plan =
        crate::shell::parse::parse_execution_plan("echo $((A + 2))", env_arc).expect("parse");
    let word = &plan.lists[0].jobs[0].stages[0].argv[1];
    match &word.parts[0] {
        WordPart::ArithmeticExpansion { expansion, quote } => {
            assert_eq!(*quote, QuoteMode::Unquoted);
            assert_eq!(expansion.source, "A + 2");
            assert!(!expansion.body.parts.is_empty());
        }
        other => panic!("expected ArithmeticExpansion, got {other:?}"),
    }

    let snapshot = ChildShellSnapshot::capture(&crate::environment::Environment::new().read());
    let request = InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot,
        kind: InternalExecKind::Plan(PlanExecRequest {
            plan,
            mode: PlanExecMode::CommandSubstitution,
            signal_policy: PlanSignalPolicy::Normal,
        }),
    };
    let bytes = serde_json::to_vec(&request).expect("encode");
    assert!(bytes.len() <= MAX_INTERNAL_EXEC_REQUEST);
    let (read, write) = crate::process::io::cloexec_pipe().expect("pipe");
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    let mut write = unsafe { std::fs::File::from_raw_fd(write.into_raw_fd()) };
    write.write_all(&bytes).expect("write");
    drop(write);
    let decoded = read_internal_request(read.into_raw_fd()).expect("decode");
    assert_eq!(decoded.version, PROTOCOL_VERSION);
    match decoded.kind {
        InternalExecKind::Plan(plan_request) => {
            let word = &plan_request.plan.lists[0].jobs[0].stages[0].argv[1];
            match &word.parts[0] {
                WordPart::ArithmeticExpansion { expansion, quote } => {
                    assert_eq!(*quote, QuoteMode::Unquoted);
                    assert_eq!(expansion.source, "A + 2");
                    assert!(!expansion.body.parts.is_empty());
                }
                other => panic!("arithmetic lost, got {other:?}"),
            }
        }
        other => panic!("expected Plan, got {other:?}"),
    }
}
#[test]
fn invocation_plan_and_snapshot_roundtrip() {
    use crate::shell::plan::{InvocationParameter, WordPart};
    let env = crate::environment::Environment::new();
    env.write().invocation = crate::environment::InvocationParameters {
        interactive: false,
        argv0: "worker".into(),
        positional: vec!["alpha".into(), "".into()],
    };
    let plan =
        crate::shell::parse::parse_execution_plan("printf '%s\n' \"$@\" \"${10}\"", env.clone())
            .unwrap();
    let request = InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot: ChildShellSnapshot::capture(&env.read()),
        kind: InternalExecKind::Plan(PlanExecRequest {
            plan,
            mode: PlanExecMode::CommandSubstitution,
            signal_policy: PlanSignalPolicy::Normal,
        }),
    };
    let decoded: InternalExecRequest =
        serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
    assert_eq!(decoded.snapshot.invocation, env.read().invocation);
    let InternalExecKind::Plan(request) = decoded.kind else {
        panic!("plan lost")
    };
    let argv = &request.plan.lists[0].jobs[0].stages[0].argv;
    assert!(matches!(
        argv[2].parts.as_slice(),
        [WordPart::InvocationParameter {
            parameter: InvocationParameter::At,
            ..
        }]
    ));
    assert!(matches!(
        argv[3].parts.as_slice(),
        [WordPart::InvocationParameter {
            parameter: InvocationParameter::Positional(10),
            ..
        }]
    ));
}
