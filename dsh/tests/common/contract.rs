//! Declarative shell-execution contract harness.
//!
//! Contracts live in `dsh/tests/spec/*.toml` and assert the shell's public
//! behavior (status, stdout/stderr, file side effects) without encoding the
//! current implementation. Known gaps are recorded as `xfail` contracts plus
//! an exact-match allowlist, so a future fix surfaces as `XPASS` instead of
//! silently rotting.
//!
//! Layering: this module only drives real `dogesh` children through
//! [`super::process`] and compares plain values. It never models lifecycle
//! states (Layer 2, proptest) and never asserts cross-process ownership
//! (Layer 3, `resource_contract`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use serde::Deserialize;

use super::process::{DEFAULT_CASE_TIMEOUT, DshTestProcess, WaitError, spawn_dsh_unlocked};

/// Spec schema version this runner understands. Unknown versions are
/// rejected rather than silently reinterpreted.
pub const CONTRACT_VERSION: u32 = 1;

/// Portable executable tokens expanded before execution. Absolute helper
/// paths differ between Linux and macOS, so specs never hardcode them.
pub fn expand_tokens(script: &str) -> String {
    let mut out = script.to_string();
    for (token, path) in [
        ("{{TRUE}}", super::true_path()),
        ("{{FALSE}}", super::false_path()),
        ("{{TR}}", super::tr_path()),
        ("{{YES}}", super::yes_path()),
        ("{{HEAD}}", super::head_path()),
        ("{{SH}}", super::sh_path()),
        ("{{KILL}}", super::kill_path()),
    ] {
        out = out.replace(token, path);
    }
    out
}

/// Reject a leftover `{{TOKEN}}` the runner does not know.
pub fn unknown_token(script: &str) -> Option<String> {
    let mut rest = script;
    while let Some(start) = rest.find("{{") {
        let tail = &rest[start..];
        let end = tail.find("}}")?;
        let token = &tail[..end + 2];
        match token {
            "{{TRUE}}" | "{{FALSE}}" | "{{TR}}" | "{{YES}}" | "{{HEAD}}" | "{{SH}}"
            | "{{KILL}}" => {
                rest = &tail[end + 2..];
            }
            _ => return Some(token.to_string()),
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContractClass {
    /// POSIX / dash-compatible behavior.
    Posix,
    /// Bash-compatible extension.
    Bash,
    /// doge-shell-specific behavior (Smart Pipe, ...).
    Dogesh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContractMode {
    /// `dogesh -c '<script>'`.
    Command,
    /// `dogesh` with `script + "\nexit\n"` on stdin.
    Interactive,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileExpectation {
    pub path: String,
    #[serde(default)]
    pub exists: Option<bool>,
    #[serde(default)]
    pub content_exact: Option<String>,
    #[serde(default)]
    pub content_contains: Vec<String>,
    #[serde(default)]
    pub content_not_contains: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ContractExpectation {
    #[serde(default)]
    pub status: Option<i32>,
    #[serde(default)]
    pub stdout_exact: Option<String>,
    #[serde(default)]
    pub stdout_contains: Vec<String>,
    #[serde(default)]
    pub stdout_not_contains: Vec<String>,
    #[serde(default)]
    pub stderr_exact: Option<String>,
    #[serde(default)]
    pub stderr_contains: Vec<String>,
    #[serde(default)]
    pub stderr_not_contains: Vec<String>,
    #[serde(default)]
    pub files: Vec<FileExpectation>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContractCase {
    pub id: String,
    pub class: ContractClass,
    pub mode: ContractMode,
    pub script: String,
    #[serde(default)]
    pub expect: ContractExpectation,
    #[serde(default)]
    pub xfail: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ContractFile {
    pub version: u32,
    pub case: Vec<ContractCase>,
}

/// Outcome of one case execution, before XFAIL handling.
#[derive(Debug)]
pub enum ContractOutcome {
    Pass,
    Fail(ContractFailure),
    Timeout(ContractFailure),
}

#[derive(Debug)]
pub struct ContractFailure {
    pub detail: String,
}

/// One case's full result line for the aggregated report.
pub struct CaseResult {
    pub id: String,
    pub outcome: ContractOutcome,
    pub xfail: Option<String>,
}

impl CaseResult {
    /// Strict outcome logic: XPASS is a failure so fixed bugs force XFAIL
    /// removal; timeouts are case failures, never panics.
    pub fn verdict(&self) -> &'static str {
        match (&self.outcome, &self.xfail) {
            (ContractOutcome::Pass, None) => "PASS",
            (ContractOutcome::Pass, Some(_)) => "XPASS",
            (ContractOutcome::Fail(_), Some(_)) | (ContractOutcome::Timeout(_), Some(_)) => "XFAIL",
            (ContractOutcome::Fail(_), None) | (ContractOutcome::Timeout(_), None) => "FAIL",
        }
    }

    pub fn is_suite_failure(&self) -> bool {
        matches!(self.verdict(), "FAIL" | "XPASS")
    }
}

/// Parse + validate one spec file (version, tokens). Execution happens in
/// [`run_case`].
pub fn load_contract_file(path: &Path) -> Result<Vec<ContractCase>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    let file: ContractFile =
        toml::from_str(&text).map_err(|err| format!("parse {}: {err}", path.display()))?;
    if file.version != CONTRACT_VERSION {
        return Err(format!(
            "{}: unknown contract version {} (runner supports {})",
            path.display(),
            file.version,
            CONTRACT_VERSION
        ));
    }
    for case in &file.case {
        if let Some(token) = unknown_token(&case.script) {
            return Err(format!(
                "{}: case {} uses unknown token {token}",
                path.display(),
                case.id
            ));
        }
    }
    Ok(file.case)
}

/// Validate a file path from `[[case.expect.files]]`: relative only, no
/// escape above the temporary cwd.
pub fn resolve_case_file(workdir: &Path, raw: &str) -> Result<PathBuf, String> {
    let rel = Path::new(raw);
    if rel.is_absolute() {
        return Err(format!("absolute file path is forbidden: {raw}"));
    }
    let mut depth: i32 = 0;
    for component in rel.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return Err(format!("file path escapes the case cwd: {raw}"));
                }
            }
            Component::Normal(_) => depth += 1,
            _ => return Err(format!("unsupported file path: {raw}")),
        }
    }
    Ok(workdir.join(rel))
}

fn check_contains(haystack: &str, needles: &[String], stream: &str, errors: &mut Vec<String>) {
    for needle in needles {
        if !haystack.contains(needle) {
            errors.push(format!(
                "{stream} does not contain {needle:?}\nactual {stream}:\n{haystack}"
            ));
        }
    }
}

fn check_not_contains(haystack: &str, needles: &[String], stream: &str, errors: &mut Vec<String>) {
    for needle in needles {
        if haystack.contains(needle) {
            errors.push(format!(
                "{stream} must not contain {needle:?}\nactual {stream}:\n{haystack}"
            ));
        }
    }
}

fn check_file_expectation(
    workdir: &Path,
    file: &FileExpectation,
    errors: &mut Vec<String>,
) -> Result<(), String> {
    let path = resolve_case_file(workdir, &file.path)?;
    let data = std::fs::read(&path);
    match (file.exists, &data) {
        (Some(false), Ok(_)) => {
            errors.push(format!("file {:?} must not exist", file.path));
        }
        (Some(false), Err(_)) => {}
        (_, Err(_))
            if file.exists == Some(true)
                || file.content_exact.is_some()
                || !file.content_contains.is_empty() =>
        {
            errors.push(format!("file {:?} must exist but is missing", file.path));
        }
        (_, Err(_)) => {}
        (_, Ok(bytes)) => {
            let text = String::from_utf8_lossy(bytes).to_string();
            if let Some(expected) = &file.content_exact
                && &text != expected
            {
                errors.push(format!(
                    "file {:?} content mismatch\nexpected:\n{expected:?}\nactual:\n{text:?}",
                    file.path
                ));
            }
            check_contains(&text, &file.content_contains, "file", errors);
            check_not_contains(&text, &file.content_not_contains, "file", errors);
        }
    }
    Ok(())
}

fn assert_output(
    case: &ContractCase,
    output: &Output,
    workdir: &Path,
) -> Result<(), ContractFailure> {
    let mut errors = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let expect = &case.expect;

    if let Some(status) = expect.status
        && output.status.code() != Some(status)
    {
        errors.push(format!(
            "status mismatch: expected {status}, got {:?}",
            output.status.code()
        ));
    }
    if let Some(expected) = &expect.stdout_exact
        && &stdout != expected
    {
        errors.push(format!(
            "stdout mismatch\nexpected:\n{expected:?}\nactual:\n{stdout:?}"
        ));
    }
    check_contains(&stdout, &expect.stdout_contains, "stdout", &mut errors);
    check_not_contains(&stdout, &expect.stdout_not_contains, "stdout", &mut errors);
    if let Some(expected) = &expect.stderr_exact
        && &stderr != expected
    {
        errors.push(format!(
            "stderr mismatch\nexpected:\n{expected:?}\nactual:\n{stderr:?}"
        ));
    }
    check_contains(&stderr, &expect.stderr_contains, "stderr", &mut errors);
    check_not_contains(&stderr, &expect.stderr_not_contains, "stderr", &mut errors);
    for file in &expect.files {
        if let Err(err) = check_file_expectation(workdir, file, &mut errors) {
            errors.push(err);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ContractFailure {
            detail: format!(
                "case {}\nscript:\n{}\nexpected status: {:?}\nactual status: {:?}\nactual stdout:\n{}\nactual stderr:\n{}\nviolations:\n- {}",
                case.id,
                case.script,
                expect.status,
                output.status.code(),
                stdout,
                stderr,
                errors.join("\n- ")
            ),
        })
    }
}

/// Execute one case in an isolated child and compare against expectations.
pub fn run_case(case: &ContractCase) -> CaseResult {
    let script = expand_tokens(&case.script);
    let timeout = case
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_CASE_TIMEOUT);
    let outcome = run_case_script(case, &script, timeout);
    CaseResult {
        id: case.id.clone(),
        outcome,
        xfail: case.xfail.clone(),
    }
}

fn run_case_script(case: &ContractCase, script: &str, timeout: Duration) -> ContractOutcome {
    // The serial lock lives in `super` and is held for the whole case so
    // contract files stay serialized against the legacy suite.
    let _guard = super::serial_guard();
    let process: DshTestProcess = match case.mode {
        ContractMode::Command => spawn_dsh_unlocked(["-c".to_string(), script.to_string()], None),
        ContractMode::Interactive => {
            let mut input = script.to_string();
            input.push_str("\nexit\n");
            spawn_dsh_unlocked(Vec::<String>::new(), Some(&input))
        }
    };
    match process.wait_keep_dirs(timeout) {
        Ok(waited) => match assert_output(case, &waited.output, &waited.workdir) {
            Ok(()) => ContractOutcome::Pass,
            Err(failure) => ContractOutcome::Fail(failure),
        },
        Err(WaitError::TimedOut(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            ContractOutcome::Timeout(ContractFailure {
                detail: format!(
                    "case {} timed out after {timeout:?}\nscript:\n{}\npartial stdout:\n{}\npartial stderr:\n{}",
                    case.id, case.script, stdout, stderr
                ),
            })
        }
        Err(WaitError::Io(err)) => ContractOutcome::Fail(ContractFailure {
            detail: format!("case {}: failed to collect output: {err}", case.id),
        }),
    }
}

/// Load the XFAIL allowlist (`spec/xfail-allowlist.txt`): one case ID per
/// line, `#` comments and blank lines ignored.
pub fn load_allowlist(path: &Path) -> Result<BTreeSet<String>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// Exact-match gate: the set of `xfail` IDs across all loaded cases must
/// equal the allowlist. New hidden XFails and stale entries both fail.
pub fn check_allowlist(
    cases_by_file: &BTreeMap<String, Vec<ContractCase>>,
    allowlist: &BTreeSet<String>,
) -> Result<(), String> {
    let mut from_specs = BTreeSet::new();
    for cases in cases_by_file.values() {
        for case in cases {
            if case.xfail.is_some() {
                from_specs.insert(case.id.clone());
            }
        }
    }
    if &from_specs == allowlist {
        return Ok(());
    }
    let mut problems = Vec::new();
    for id in from_specs.difference(allowlist) {
        problems.push(format!("xfail case {id:?} is missing from the allowlist"));
    }
    for id in allowlist.difference(&from_specs) {
        problems.push(format!("allowlist entry {id:?} has no xfail case"));
    }
    Err(problems.join("\n"))
}

/// Case IDs must be unique across every spec file.
pub fn check_duplicate_ids(
    cases_by_file: &BTreeMap<String, Vec<ContractCase>>,
) -> Result<(), String> {
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    let mut duplicates = Vec::new();
    for (file, cases) in cases_by_file {
        for case in cases {
            if let Some(first) = seen.insert(case.id.as_str(), file.as_str()) {
                duplicates.push(format!(
                    "duplicate case id {:?} in {first} and {file}",
                    case.id
                ));
            }
        }
    }
    if duplicates.is_empty() {
        Ok(())
    } else {
        Err(duplicates.join("\n"))
    }
}

/// Render aggregated failures so one bad case never hides the rest.
pub fn render_results(results: &[CaseResult]) -> String {
    let mut out = String::new();
    let (mut pass, mut xfail, mut fail, mut xpass) = (0, 0, 0, 0);
    for result in results {
        match result.verdict() {
            "PASS" => pass += 1,
            "XFAIL" => xfail += 1,
            "FAIL" => fail += 1,
            _ => xpass += 1,
        }
    }
    out.push_str(&format!(
        "contract results: {pass} PASS, {xfail} XFAIL, {fail} FAIL, {xpass} XPASS\n"
    ));
    for result in results {
        match &result.outcome {
            ContractOutcome::Pass => {
                if result.xfail.is_some() {
                    out.push_str(&format!(
                        "\n[XPASS] {}: passed but is marked xfail ({:?}); remove the xfail marker\n",
                        result.id, result.xfail
                    ));
                }
            }
            ContractOutcome::Fail(failure) | ContractOutcome::Timeout(failure) => {
                out.push_str(&format!(
                    "\n[{}] {}\n{}\n",
                    result.verdict(),
                    result.id,
                    failure.detail
                ));
            }
        }
    }
    out
}

/// Optional differential mode: replay `Posix` cases against reference
/// shells for investigation only. Never authoritative; never fails normal
/// CI when a reference shell is missing.
///
/// Enabled with `DOGESH_CONTRACT_COMPARE_SHELLS=1`.
pub fn compare_with_reference_shells(case: &ContractCase) -> Option<String> {
    if std::env::var("DOGESH_CONTRACT_COMPARE_SHELLS")
        .ok()
        .as_deref()
        != Some("1")
    {
        return None;
    }
    if case.class != ContractClass::Posix {
        return None;
    }
    let script = expand_tokens(&case.script);
    // Reference shells run in a throwaway cwd so their file side effects
    // never pollute the test process's directory.
    let scratch = tempfile::TempDir::new().expect("differential scratch dir");
    let mut report = format!("differential case {}\nscript:\n{script}\n", case.id);
    for shell in ["bash", "sh", "dash"] {
        let probing = std::process::Command::new(shell)
            .arg("-c")
            .arg(&script)
            .current_dir(scratch.path())
            .output();
        match probing {
            Ok(output) => {
                report.push_str(&format!(
                    "\n[{shell}] status={:?}\nstdout:\n{}\nstderr:\n{}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            Err(_) => report.push_str(&format!("\n[{shell}] missing, skipped\n")),
        }
    }
    Some(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case_with_id(id: &str, xfail: Option<&str>) -> ContractCase {
        ContractCase {
            id: id.to_string(),
            class: ContractClass::Posix,
            mode: ContractMode::Command,
            script: "true".to_string(),
            expect: ContractExpectation::default(),
            xfail: xfail.map(str::to_string),
            timeout_ms: None,
        }
    }

    #[test]
    fn tokens_expand_to_absolute_paths() {
        let script =
            expand_tokens("{{TRUE}} | {{FALSE}} | {{TR}} | {{YES}} | {{HEAD}} | {{SH}} | {{KILL}}");
        assert!(!script.contains("{{"));
        assert!(!script.contains("}}"));
        for path in script.split(" | ") {
            assert!(Path::new(path).is_absolute(), "{path} is not absolute");
            assert!(Path::new(path).exists(), "{path} does not exist");
        }
    }

    #[test]
    fn unknown_token_is_reported() {
        assert_eq!(unknown_token("echo {{NOPE}}").as_deref(), Some("{{NOPE}}"));
        assert_eq!(unknown_token("{{TRUE}} ok").as_deref(), None);
        assert_eq!(unknown_token("{{KILL}} ok").as_deref(), None);
    }

    #[test]
    fn absolute_and_escaping_file_paths_rejected() {
        let workdir = Path::new("/tmp/work");
        assert!(resolve_case_file(workdir, "/abs/out.txt").is_err());
        assert!(resolve_case_file(workdir, "../escape.txt").is_err());
        assert!(resolve_case_file(workdir, "sub/../../escape.txt").is_err());
        assert!(resolve_case_file(workdir, "sub/out.txt").is_ok());
    }

    #[test]
    fn xpass_is_a_suite_failure() {
        let result = CaseResult {
            id: "x".to_string(),
            outcome: ContractOutcome::Pass,
            xfail: Some("reason".to_string()),
        };
        assert_eq!(result.verdict(), "XPASS");
        assert!(result.is_suite_failure());
    }

    #[test]
    fn xfail_timeout_is_not_a_suite_failure() {
        let failure = ContractFailure {
            detail: "boom".to_string(),
        };
        let result = CaseResult {
            id: "x".to_string(),
            outcome: ContractOutcome::Timeout(failure),
            xfail: Some("known deadlock".to_string()),
        };
        assert_eq!(result.verdict(), "XFAIL");
        assert!(!result.is_suite_failure());
    }

    #[test]
    fn duplicate_ids_rejected_across_files() {
        let mut map = BTreeMap::new();
        map.insert("a.toml".to_string(), vec![case_with_id("same", None)]);
        map.insert("b.toml".to_string(), vec![case_with_id("same", None)]);
        assert!(check_duplicate_ids(&map).is_err());
    }

    #[test]
    fn allowlist_requires_exact_match() {
        let mut map = BTreeMap::new();
        map.insert(
            "a.toml".to_string(),
            vec![case_with_id("known", Some("reason"))],
        );
        // Missing entry.
        assert!(check_allowlist(&map, &BTreeSet::new()).is_err());
        // Stale entry.
        let mut stale = BTreeSet::new();
        stale.insert("known".to_string());
        stale.insert("gone".to_string());
        assert!(check_allowlist(&map, &stale).is_err());
        // Exact match.
        let mut exact = BTreeSet::new();
        exact.insert("known".to_string());
        assert!(check_allowlist(&map, &exact).is_ok());
    }

    #[test]
    fn unknown_version_rejected() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "version = 99\n").expect("write spec");
        assert!(load_contract_file(&path).is_err());
    }
}
