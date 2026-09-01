use crate::ShellProxy;
use crate::safety_policy;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read};

pub(crate) const NAME: &str = "search";

const DEFAULT_MAX_RESULTS: usize = 50;
const MAX_MAX_RESULTS: usize = 200;
/// Matching lines reported per file for a content search. One line per file
/// hides the other call sites the model is looking for.
const MAX_MATCHES_PER_FILE: usize = 5;
/// Ceiling on the rendered result, checked before the shared tool-output cap.
/// Without it, two hundred long matching lines were cut from the middle by the
/// generic truncator and the model saw half a result list with no marker.
const MAX_OUTPUT_CHARS: usize = 6144;
/// A matching line longer than this is reported trimmed. Minified files and
/// lockfiles otherwise spend the whole budget on one line.
const MAX_LINE_CHARS: usize = 300;
/// How much of a file is inspected to decide whether it is text.
const BINARY_SNIFF_BYTES: usize = 8192;

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Search for files by name or content. Content search takes a plain substring by default, or a regular expression with `regex: true`.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query: a glob for `filename`, or a substring (or regular expression) for `content`"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path to start search from (relative to current directory or absolute for skills)"
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of results to return. Defaults to 50."
                    },
                    "type": {
                        "type": "string",
                        "enum": ["filename", "content"],
                        "description": "Type of search: 'filename' (glob pattern) or 'content' (text search)"
                    },
                    "regex": {
                        "type": "boolean",
                        "description": "Treat `query` as a regular expression. Content search only. Defaults to false."
                    },
                    "ignore_case": {
                        "type": "boolean",
                        "description": "Match without regard to case. Defaults to false."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Only search files whose name matches this glob, e.g. `*.rs`. Content search only."
                    }
                },
                "required": ["query", "type"],
                "additionalProperties": false
            }
        }
    })
}

pub(crate) fn run(arguments: &str, _proxy: &mut dyn ShellProxy) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for search tool: {err}"))?;

    let query = parsed
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: search tool requires `query`".to_string())?;

    let search_type = parsed
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: search tool requires `type`".to_string())?;

    let path_value = parsed.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let normalized_abs_path = super::resolve_tool_path(path_value, _proxy)?;

    // Get CWD for output stripping
    let current_dir = _proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;
    let normalized_current_dir =
        std::fs::canonicalize(&current_dir).unwrap_or_else(|_| super::normalize_path(&current_dir));

    if !normalized_abs_path.exists() {
        return Err(format!("chat: path `{path_value}` does not exist"));
    }

    super::reject_gitignored_path(&normalized_abs_path, &normalized_current_dir, path_value)?;

    if let Some(reason) = super::sensitive_path_reason(&normalized_abs_path)
        && !super::confirm_sensitive_access(_proxy, "search", path_value, reason)?
    {
        return Ok("search cancelled by user.".to_string());
    }

    let mut results = Vec::new();
    let max_results = parsed
        .get("max_results")
        .and_then(|v| v.as_u64())
        .map(|v| (v.max(1) as usize).min(MAX_MAX_RESULTS))
        .unwrap_or(DEFAULT_MAX_RESULTS);
    let mut hidden_sensitive = 0usize;

    match search_type {
        "filename" => {
            let glob_pattern = format!("**/{}", query);
            let glob = glob::Pattern::new(&glob_pattern)
                .map_err(|err| format!("chat: invalid glob pattern: {err}"))?;

            // Use ignore::WalkBuilder to automatically respect .gitignore
            for entry in WalkBuilder::new(&normalized_abs_path)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .build()
                .filter_map(|e| e.ok())
            {
                if results.len() >= max_results {
                    break;
                }

                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }
                if safety_policy::is_sensitive_path(entry.path()) {
                    hidden_sensitive += 1;
                    continue;
                }

                // Check if file matches glob
                // We match against the relative path from the search root
                if let Ok(_rel_path) = entry.path().strip_prefix(&normalized_abs_path) {
                    // For glob matching, we want to match against the filename or path
                    // Simple implementation: check if filename matches
                    if glob.matches_path(entry.path())
                        || entry
                            .file_name()
                            .to_str()
                            .map(|s| glob.matches(s))
                            .unwrap_or(false)
                    {
                        // Get path relative to CWD for output
                        if let Ok(cwd_rel) = entry.path().strip_prefix(&normalized_current_dir) {
                            results.push(cwd_rel.display().to_string());
                        }
                    }
                }
            }
        }
        "content" => {
            let matcher = ContentMatcher::build(&parsed, query)?;
            let name_filter = parsed
                .get("glob")
                .and_then(|value| value.as_str())
                .filter(|pattern| !pattern.trim().is_empty())
                .map(|pattern| {
                    glob::Pattern::new(pattern)
                        .map_err(|err| format!("chat: invalid `glob` pattern: {err}"))
                })
                .transpose()?;

            // Use ignore::WalkBuilder to automatically respect .gitignore
            for entry in WalkBuilder::new(&normalized_abs_path)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .build()
                .filter_map(|e| e.ok())
            {
                if results.len() >= max_results {
                    break;
                }

                if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    continue;
                }
                if safety_policy::is_sensitive_path(entry.path()) {
                    hidden_sensitive += 1;
                    continue;
                }
                if let Some(filter) = &name_filter
                    && !entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| filter.matches(name))
                {
                    continue;
                }

                // Reading a binary file line by line produced no matches and
                // cost the whole file; the previous code left this as a to-do.
                if looks_binary(entry.path()) {
                    continue;
                }

                if let Ok(file) = fs::File::open(entry.path()) {
                    let reader = BufReader::new(file);
                    let mut matches_in_file = 0usize;
                    for (line_idx, line) in reader.lines().enumerate() {
                        if results.len() >= max_results || matches_in_file >= MAX_MATCHES_PER_FILE {
                            break;
                        }
                        if let Ok(line_content) = line
                            && matcher.is_match(&line_content)
                            && let Ok(cwd_rel) = entry.path().strip_prefix(&normalized_current_dir)
                        {
                            let line_content =
                                safety_policy::redact_sensitive_text(line_content.trim());
                            results.push(format!(
                                "{}:{}: {}",
                                cwd_rel.display(),
                                line_idx + 1,
                                trim_line(&line_content)
                            ));
                            matches_in_file += 1;
                        }
                    }
                }
            }
        }
        _ => return Err(format!("chat: unsupported search type `{search_type}`")),
    }

    let mut output = String::new();
    output.push_str(&format!(
        "Search results for `{}` in `{}`:\n",
        query, path_value
    ));

    if results.is_empty() {
        output.push_str("(no matches found)");
    } else {
        let hit_cap = results.len() >= max_results;
        for result in results {
            output.push_str(&format!("- {}\n", result));
        }
        if hit_cap {
            output.push_str(&format!(
                "... (stopped at max_results={max_results}; narrow the query or raise max_results)"
            ));
        }
    }
    if hidden_sensitive > 0 {
        output.push_str(&format!(
            "\n... ({hidden_sensitive} sensitive paths hidden)"
        ));
    }

    // Cut here rather than leaving it to the shared cap: that one takes the
    // middle out, which in a list of matches silently drops results with no
    // marker where they were.
    Ok(cap_output(output))
}

/// Keep the head of the result list and say what was dropped.
fn cap_output(output: String) -> String {
    if output.len() <= MAX_OUTPUT_CHARS {
        return output;
    }
    let end = output.floor_char_boundary(MAX_OUTPUT_CHARS);
    let kept = &output[..end];
    let shown = kept.lines().count();
    format!(
        "{kept}\n... (output truncated after ~{shown} lines; narrow the query, pass `glob`, or lower max_results)"
    )
}

// Helper function to normalize a path by resolving all relative components

/// How a content search decides whether a line matches.
enum ContentMatcher {
    /// Plain substring, the default. Cheaper than a regex and what most callers
    /// mean.
    Substring(String),
    CaseInsensitiveSubstring(String),
    Regex(regex::Regex),
}

impl ContentMatcher {
    fn build(parsed: &Value, query: &str) -> Result<Self, String> {
        let ignore_case = parsed
            .get("ignore_case")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let use_regex = parsed
            .get("regex")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if use_regex {
            let regex = RegexBuilder::new(query)
                .case_insensitive(ignore_case)
                .build()
                .map_err(|err| format!("chat: invalid regular expression `{query}`: {err}"))?;
            return Ok(Self::Regex(regex));
        }

        Ok(if ignore_case {
            Self::CaseInsensitiveSubstring(query.to_lowercase())
        } else {
            Self::Substring(query.to_string())
        })
    }

    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Substring(needle) => line.contains(needle.as_str()),
            Self::CaseInsensitiveSubstring(needle) => line.to_lowercase().contains(needle.as_str()),
            Self::Regex(regex) => regex.is_match(line),
        }
    }
}

/// Whether a file is binary, judged the way `grep` judges it: a NUL byte in the
/// first few kilobytes.
fn looks_binary(path: &std::path::Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut buffer = [0u8; BINARY_SNIFF_BYTES];
    match file.read(&mut buffer) {
        Ok(read) => buffer[..read].contains(&0),
        Err(_) => false,
    }
}

/// Keep a long matching line from eating the whole result budget.
fn trim_line(line: &str) -> String {
    if line.len() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let end = line.floor_char_boundary(MAX_LINE_CHARS);
    format!("{}... (line truncated)", &line[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use crate::test_support::TestShellProxy;
    type NoopProxy = TestShellProxy;

    #[test]
    fn test_search_filename() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test_file.rs");
        fs::write(&file_path, "content").unwrap();

        let mut proxy = proxy(dir.path());

        let result = run(
            r#"{"query": "test_file.rs", "type": "filename"}"#,
            &mut proxy,
        )
        .unwrap();
        assert!(result.contains("test_file.rs"));
    }

    #[test]
    fn test_search_content() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let mut proxy = proxy(dir.path());

        let result = run(r#"{"query": "world", "type": "content"}"#, &mut proxy).unwrap();
        assert!(result.contains("test.txt"));
        assert!(result.contains("hello world"));
    }

    #[test]
    fn test_search_rejects_parent_traversal() {
        let dir = tempdir().unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(
            r#"{"query": "secret", "type": "content", "path": "../"}"#,
            &mut proxy,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed directories"));
    }

    #[cfg(unix)]
    #[test]
    fn test_search_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let base = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir_all(base.path().join("inside")).unwrap();
        symlink(outside.path(), base.path().join("inside/link_out")).unwrap();
        let mut proxy = proxy(base.path());

        let result = run(
            r#"{"query": "secret", "type": "content", "path": "inside/link_out"}"#,
            &mut proxy,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed directories"));
    }

    fn proxy(path: &std::path::Path) -> NoopProxy {
        NoopProxy {
            current_dir: path.to_path_buf(),
            confirm_result: true,
            ..NoopProxy::default()
        }
    }

    /// Substring search cannot express "one of these two", so the model was
    /// reduced to several calls or to reading whole files.
    #[test]
    fn content_search_accepts_a_regular_expression() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(
            r#"{"query": "fn (alpha|beta)", "type": "content", "regex": true}"#,
            &mut proxy,
        )
        .unwrap();

        assert!(result.contains("fn alpha()"), "{result}");
        assert!(result.contains("fn beta()"), "{result}");
    }

    #[test]
    fn an_invalid_regular_expression_is_reported_not_matched_literally() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        let mut proxy = proxy(dir.path());

        let err = run(
            r#"{"query": "fn (", "type": "content", "regex": true}"#,
            &mut proxy,
        )
        .expect_err("an unparseable pattern must be reported");

        assert!(err.contains("invalid regular expression"), "{err}");
    }

    #[test]
    fn content_search_can_ignore_case() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "Struct Widget;\n").unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(
            r#"{"query": "struct widget", "type": "content", "ignore_case": true}"#,
            &mut proxy,
        )
        .unwrap();

        assert!(result.contains("Struct Widget"), "{result}");
    }

    #[test]
    fn a_glob_restricts_which_files_are_read() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("keep.rs"), "needle\n").unwrap();
        fs::write(dir.path().join("skip.txt"), "needle\n").unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(
            r#"{"query": "needle", "type": "content", "glob": "*.rs"}"#,
            &mut proxy,
        )
        .unwrap();

        assert!(result.contains("keep.rs"), "{result}");
        assert!(!result.contains("skip.txt"), "{result}");
    }

    /// A binary file used to be read line by line: no matches, all the cost.
    #[test]
    fn binary_files_are_skipped() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("blob.bin"), b"needle\x00\x01\x02needle").unwrap();
        fs::write(dir.path().join("plain.txt"), "needle\n").unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(r#"{"query": "needle", "type": "content"}"#, &mut proxy).unwrap();

        assert!(result.contains("plain.txt"), "{result}");
        assert!(!result.contains("blob.bin"), "{result}");
    }

    /// A minified line must not spend the entire result budget.
    #[test]
    fn a_very_long_matching_line_is_trimmed() {
        let dir = tempdir().unwrap();
        let long = format!("needle{}", "x".repeat(MAX_LINE_CHARS * 2));
        fs::write(dir.path().join("min.js"), &long).unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(r#"{"query": "needle", "type": "content"}"#, &mut proxy).unwrap();

        assert!(result.contains("line truncated"), "{result}");
        assert!(result.len() < long.len(), "{}", result.len());
    }

    /// The cap keeps the head, so the first results survive intact.
    #[test]
    fn capping_keeps_the_head_and_says_so() {
        let output = format!("header\n{}", "- some/long/path.rs:1: match\n".repeat(500));

        let capped = cap_output(output);

        assert!(capped.starts_with("header\n"), "{capped}");
        assert!(capped.contains("output truncated"), "{capped}");
        assert!(capped.len() < MAX_OUTPUT_CHARS + 200);
    }

    #[test]
    fn test_search_redacts_sensitive_content_results() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("config.txt"),
            "call me with Authorization: Bearer secret-token",
        )
        .unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(
            r#"{"query": "Authorization", "type": "content"}"#,
            &mut proxy,
        )
        .unwrap();

        assert!(result.contains("Authorization: Bearer ***"));
        assert!(!result.contains("secret-token"));
    }

    #[test]
    fn test_search_hides_sensitive_paths() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("deploy.key"), "API_KEY=secret").unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(r#"{"query": "API_KEY", "type": "content"}"#, &mut proxy).unwrap();

        assert!(!result.contains(".env.local"));
        assert!(result.contains("sensitive paths hidden"));
    }
}
