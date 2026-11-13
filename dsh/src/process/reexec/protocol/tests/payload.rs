use super::super::*;
use std::os::fd::FromRawFd as _;

fn pipeline_source_request(data: &str) -> InternalExecRequest {
    let env_arc = crate::environment::Environment::new();
    InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot: ChildShellSnapshot::capture(&env_arc.read()),
        kind: InternalExecKind::PipelineSource(PipelineSourceExecRequest {
            data: data.to_string(),
        }),
    }
}

fn roundtrip_data(data: &str) -> String {
    let request = pipeline_source_request(data);
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
        InternalExecKind::PipelineSource(source) => source.data,
        other => panic!("expected PipelineSource, got {other:?}"),
    }
}

#[test]
fn pipeline_source_roundtrips_exact_bytes() {
    assert_eq!(roundtrip_data("hello\n"), "hello\n");
    assert_eq!(roundtrip_data(""), "");
    assert_eq!(roundtrip_data("ABC\n"), "ABC\n");
}

fn no_command_request(
    assignments: Vec<(String, String)>,
    last_command_substitution_status: Option<i32>,
) -> InternalExecRequest {
    let env_arc = crate::environment::Environment::new();
    InternalExecRequest {
        version: PROTOCOL_VERSION,
        snapshot: ChildShellSnapshot::capture(&env_arc.read()),
        kind: InternalExecKind::NoCommand(NoCommandExecRequest {
            assignments,
            last_command_substitution_status,
        }),
    }
}

fn roundtrip_no_command(
    assignments: Vec<(String, String)>,
    last_command_substitution_status: Option<i32>,
) -> (Vec<(String, String)>, Option<i32>) {
    let request = no_command_request(assignments, last_command_substitution_status);
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
        InternalExecKind::NoCommand(no_command) => (
            no_command.assignments,
            no_command.last_command_substitution_status,
        ),
        other => panic!("expected NoCommand, got {other:?}"),
    }
}

#[test]
fn no_command_roundtrips_assignments_and_status() {
    // Spaces and Unicode in values must survive the JSON payload.
    let assignments = vec![
        ("FOO".to_string(), "bar baz".to_string()),
        ("UNICODE".to_string(), "日本語 ☃".to_string()),
        ("EMPTY".to_string(), String::new()),
    ];
    assert_eq!(
        roundtrip_no_command(assignments.clone(), Some(7)),
        (assignments, Some(7))
    );
    assert_eq!(roundtrip_no_command(vec![], None), (vec![], None));
}

#[test]
fn pipeline_source_roundtrips_large_payload() {
    let large = "y\n".repeat(70_000);
    let request = pipeline_source_request(&large);
    let bytes = serde_json::to_vec(&request).expect("encode");
    assert!(bytes.len() <= MAX_INTERNAL_EXEC_REQUEST);
    let decoded: InternalExecRequest = serde_json::from_slice(&bytes).expect("decode without pipe");
    match decoded.kind {
        InternalExecKind::PipelineSource(source) => assert_eq!(source.data, large),
        other => panic!("expected PipelineSource, got {other:?}"),
    }
}
