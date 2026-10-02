//! `observation_read`: recover an offloaded tool result without re-running it.
//!
//! Reads only [`super::super::observation::ObservationStore`] for the current
//! conversation. Never dispatches `read_file`, `search`, `execute`, MCP, or
//! job tools behind the scenes; an unknown id is an explicit error, never a
//! fallback execution.
use serde_json::{Value, json};

use crate::shell_capabilities::ChatToolHost;

pub(crate) const NAME: &str = "observation_read";

/// Default window when the caller does not ask for one.
pub(crate) const DEFAULT_READ_BYTES: usize = 4096;
/// Ceiling on one retrieval window. Fits below the global tool-output cap
/// (`MAX_TOOL_OUTPUT_CHARS`, 8192) after the header below is added.
pub(crate) const MAX_READ_BYTES: usize = 6144;

pub(crate) fn definition() -> Value {
    crate::agent::definition(
        NAME,
        "Retrieve a tool result that history says was offloaded. Use this instead of rerunning the original operation merely to recover its old output. Takes an observation id from history (obs-000001) with byte offset/limit paging.",
        json!({
            "id": {"type": "string", "description": "Observation id from history, e.g. obs-000001."},
            "offset": {"type": "integer", "minimum": 0, "description": "Byte offset into the stored result. Defaults to 0."},
            "limit": {"type": "integer", "minimum": 1, "maximum": 6144, "description": "Maximum bytes to return. Defaults to 4096."}
        }),
        &["id"],
    )
}

/// Render one stored window with its paging header.
pub(crate) fn render_window(
    id: &str,
    start: usize,
    end: usize,
    total: usize,
    window: &str,
) -> String {
    let mut out = format!("observation {id}: bytes {start}-{end} of {total}\n{window}");
    if end < total {
        out.push_str(&format!(
            "\n\n... continue with observation_read(id=\"{id}\", offset={end})"
        ));
    }
    out
}

/// Parse `observation_read` arguments. Returns `(id, offset, limit)`.
///
/// A present-but-malformed `offset`/`limit` (negative, float, string) is an
/// error rather than a silent default: quietly restarting at page zero hides
/// a model typo behind a repeated first page.
pub(crate) fn parse_arguments(arguments: &str) -> Result<(String, usize, usize), String> {
    let args: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for {NAME} tool: {err}"))?;
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("chat: {NAME} tool requires `id`"))?
        .to_string();
    let offset = match args.get("offset") {
        None => 0,
        Some(value) => value.as_u64().ok_or_else(|| {
            format!("chat: invalid `offset` for {NAME} tool: must be an integer >= 0")
        })? as usize,
    };
    let limit = match args.get("limit") {
        None => DEFAULT_READ_BYTES,
        Some(value) => {
            let raw = value.as_u64().ok_or_else(|| {
                format!("chat: invalid `limit` for {NAME} tool: must be an integer >= 1")
            })? as usize;
            if raw < 1 {
                return Err(format!(
                    "chat: invalid `limit` for {NAME} tool: must be an integer >= 1"
                ));
            }
            raw.clamp(1, MAX_READ_BYTES)
        }
    };
    Ok((id, offset, limit))
}

/// Run `observation_read` against one conversation's store.
///
/// Threaded through the normal tool seam (`execute_tool_call` /
/// `dispatch_tool`), so hooks, logging, and task bookkeeping apply exactly
/// as for every other tool. Read-only: never requires `task_plan`.
pub(crate) fn run(
    arguments: &str,
    _proxy: &mut dyn ChatToolHost,
    store: &super::super::observation::ObservationStore,
) -> Result<String, String> {
    let (id, offset, limit) = parse_arguments(arguments)?;
    let (start, end, total, window) = store
        .read_window(&id, offset, limit)
        .map_err(|err| err.to_string())?;
    Ok(render_window(&id, start, end, total, &window))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatgpt::observation::ObservationStore;
    use crate::test_support::TestShellProxy;

    fn store_with(content: &str) -> (ObservationStore, String) {
        let mut store = ObservationStore::default();
        let id = store
            .insert(
                "call-1",
                "read_file",
                content.to_string(),
                content.len() + 100,
            )
            .unwrap();
        (store, id)
    }

    #[test]
    fn observation_read_returns_exact_window_with_header() {
        let (store, id) = store_with(&"a".repeat(5000));
        let mut proxy = TestShellProxy::default();
        let out = run(&format!(r#"{{"id":"{id}"}}"#), &mut proxy, &store).unwrap();
        assert!(out.contains(&format!("observation {id}: bytes 0-")));
        assert!(out.contains(&format!("offset={DEFAULT_READ_BYTES}")));
    }

    #[test]
    fn observation_read_paging_reassembles_losslessly() {
        let content = "x".repeat(10000);
        let (store, id) = store_with(&content);
        let mut proxy = TestShellProxy::default();
        let mut reassembled = String::new();
        let mut offset = 0usize;
        loop {
            let out = run(
                &format!(r#"{{"id":"{id}","offset":{offset},"limit":2000}}"#),
                &mut proxy,
                &store,
            )
            .unwrap();
            // Strip the first header line; the remainder is the window plus an
            // optional continuation note.
            let mut parts = out.splitn(2, '\n');
            parts.next();
            let rest = parts.next().unwrap_or_default();
            let window = rest.split("\n\n... continue").next().unwrap_or_default();
            reassembled.push_str(window);
            let (_, _, total, _) = store.read_window(&id, offset, 2000).unwrap();
            let (_, end, _, _) = store.read_window(&id, offset, 2000).unwrap();
            if end >= total {
                break;
            }
            offset = end;
            assert!(offset < total);
        }
        assert_eq!(reassembled, content);
    }

    #[test]
    fn observation_read_rejects_unknown_id_without_running_tools() {
        let store = ObservationStore::default();
        let mut proxy = TestShellProxy::default();
        let err = run(r#"{"id":"obs-999999"}"#, &mut proxy, &store).unwrap_err();
        assert!(err.contains("unknown observation"));
    }

    #[test]
    fn observation_read_clamps_limit_to_max() {
        let (id, offset, limit) = parse_arguments(r#"{"id":"obs-000001","limit":999999}"#).unwrap();
        assert_eq!(id, "obs-000001");
        assert_eq!(offset, 0);
        assert_eq!(limit, MAX_READ_BYTES);
    }
}

#[cfg(test)]
mod argument_validation_tests {
    use super::*;

    #[test]
    fn observation_read_rejects_malformed_paging_arguments() {
        for arguments in [
            r#"{"id":"obs-000001","offset":-1}"#,
            r#"{"id":"obs-000001","offset":"ten"}"#,
            r#"{"id":"obs-000001","offset":1.5}"#,
            r#"{"id":"obs-000001","limit":0}"#,
            r#"{"id":"obs-000001","limit":-3}"#,
            r#"{"id":"obs-000001","limit":"many"}"#,
        ] {
            assert!(
                parse_arguments(arguments).is_err(),
                "must reject: {arguments}"
            );
        }
        // Absent paging args still default; explicit zero-limit is an error,
        // not a silent first page.
        let (_, offset, limit) = parse_arguments(r#"{"id":"obs-000001"}"#).unwrap();
        assert_eq!((offset, limit), (0, DEFAULT_READ_BYTES));
    }
}
