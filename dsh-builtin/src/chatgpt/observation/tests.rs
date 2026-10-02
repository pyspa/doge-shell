//! Observation Store unit tests: round trip, paging, capacity, and ID stability.
use super::*;

#[test]
fn observation_store_round_trip_preserves_exact_content() {
    let mut store = ObservationStore::default();
    let content = "x".repeat(5000);
    let id = store
        .insert("call-1", "read_file", content.clone(), 5100)
        .expect("fits");
    assert_eq!(id, "obs-000001");
    let entry = store.get(&id).expect("stored");
    assert_eq!(entry.content, content);
    assert_eq!(entry.tool_name, "read_file");
    assert_eq!(entry.tool_call_id, "call-1");
}

#[test]
fn observation_ids_are_monotonic_and_opaque() {
    let mut store = ObservationStore::default();
    let first = store.insert("a", "search", "one".into(), 100).unwrap();
    let second = store.insert("b", "search", "two".into(), 100).unwrap();
    assert_eq!(first, "obs-000001");
    assert_eq!(second, "obs-000002");
    assert!(!first.contains("search"));
    assert!(!first.contains("one"));
}

#[test]
fn observation_unicode_paging_reassembles_losslessly() {
    let content = format!("日本語{}絵文字🎉{}", "あ".repeat(1000), "x".repeat(5000));
    let mut store = ObservationStore::default();
    let id = store
        .insert("call-u", "read_file", content.clone(), content.len() + 100)
        .unwrap();
    let mut reassembled = String::new();
    let mut offset = 0usize;
    loop {
        let (start, end, total, window) = store.read_window(&id, offset, 1024).unwrap();
        assert_eq!(total, content.len());
        assert!(content.is_char_boundary(start));
        assert!(content.is_char_boundary(end));
        reassembled.push_str(&window);
        if end >= total {
            break;
        }
        assert!(end > offset);
        offset = end;
    }
    assert_eq!(reassembled, content);
}

#[test]
fn observation_paging_never_splits_a_code_point() {
    let content = "あいうえお".repeat(1000);
    let mut store = ObservationStore::default();
    let id = store
        .insert("call-e", "search", content.clone(), content.len() + 50)
        .unwrap();
    // Offset landing inside a 3-byte hiragana char must round up, never panic.
    let byte_inside = 1usize;
    let (start, end, total, window) = store.read_window(&id, byte_inside, 10).unwrap();
    assert_eq!(total, content.len());
    assert!(content.is_char_boundary(start));
    assert!(content.is_char_boundary(end));
    assert!(window.len() <= 10 || content[start..end].len() == window.len());
}

#[test]
fn observation_unknown_id_is_an_explicit_error() {
    let store = ObservationStore::default();
    assert!(store.read_window("obs-999999", 0, 100).is_err());
}

#[test]
fn observation_store_respects_entry_and_byte_limits() {
    let mut store = ObservationStore::default();
    // Single oversized entry refuses.
    let huge = "z".repeat(MAX_SINGLE_OBSERVATION_BYTES + 1);
    assert!(store.insert("big", "tool_describe", huge, 10).is_none());
    // Fill to entry limit with tiny entries would take 256 inserts; instead
    // fill byte budget with 8 KiB entries (128 of them = 1 MiB).
    let mut inserted = 0usize;
    for index in 0..300 {
        let content = "y".repeat(8192);
        match store.insert(&format!("c{index}"), "read_file", content, 8300) {
            Some(_) => inserted += 1,
            None => break,
        }
    }
    assert!(inserted > 0);
    assert!(inserted <= MAX_OBSERVATION_ENTRIES);
    assert!(store.stored_content_bytes() <= MAX_OBSERVATION_STORE_BYTES);
    // No eviction: first id still resolves.
    assert!(store.get("obs-000001").is_some());
}

#[test]
fn observation_stub_round_trips_its_id() {
    let stub = observation_stub(
        "read_file",
        6124,
        "obs-000123",
        ObservationReason::Historical,
    );
    assert!(stub.contains("read_file"));
    assert!(stub.contains("6124"));
    assert!(stub.contains("obs-000123"));
    assert!(stub.contains("observation_read"));
    assert_eq!(parse_observation_stub(&stub).as_deref(), Some("obs-000123"));
    assert!(!parse_observation_stub("plain result").is_some());
    assert!(
        !parse_observation_stub("(elided: search, 100 bytes; call it again if you need it)")
            .is_some()
    );
}

#[test]
fn observation_small_limit_over_multibyte_text_always_makes_progress() {
    let content = "あいうえお🎉".repeat(200);
    let mut store = ObservationStore::default();
    let id = store
        .insert("call-s", "search", content.clone(), content.len() + 50)
        .unwrap();
    // limit=1 from every mid-character offset must still advance: an empty
    // window at the same offset would stall paging forever.
    let mut reassembled = String::new();
    let mut offset = 0usize;
    let mut steps = 0usize;
    while offset < content.len() {
        let (start, end, total, window) = store.read_window(&id, offset, 1).unwrap();
        assert_eq!(total, content.len());
        assert!(content.is_char_boundary(start));
        assert!(content.is_char_boundary(end));
        assert!(end > offset, "no progress at offset {offset}");
        reassembled.push_str(&window);
        offset = end;
        steps += 1;
        assert!(steps < content.len() + 10, "paging did not terminate");
    }
    assert_eq!(reassembled, content);
}
