//! Layer 1: declarative shell-execution contract suite.
//!
//! Each `spec/*.toml` file is driven by one `#[test]` that runs every case
//! and reports all failures together, so a single bad case never hides the
//! rest of the file's diagnostics. Known gaps are `xfail` cases gated by an
//! exact-match allowlist (`spec/xfail-allowlist.txt`): a fix surfaces as
//! `XPASS` (suite failure) instead of silent rot.
//!
//! Existing example-based tests are untouched; these contracts specify
//! public shell behavior, not implementation regressions.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::contract::{
    CaseResult, ContractCase, check_allowlist, check_duplicate_ids, compare_with_reference_shells,
    load_allowlist, load_contract_file, render_results, run_case,
};

fn spec_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("spec")
}

fn spec_files() -> Vec<(String, PathBuf)> {
    let dir = spec_dir();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("read spec dir {}: {err}", dir.display()))
        .map(|entry| entry.expect("spec dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    entries.sort();
    entries
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .expect("spec file name")
                .to_string_lossy()
                .to_string();
            (name, path)
        })
        .collect()
}

fn load_all_cases() -> BTreeMap<String, Vec<ContractCase>> {
    let mut map = BTreeMap::new();
    for (name, path) in spec_files() {
        match load_contract_file(&path) {
            Ok(cases) => {
                map.insert(name, cases);
            }
            Err(err) => panic!("invalid contract spec: {err}"),
        }
    }
    map
}

/// Run every case of one spec file; collect failures and panic once.
fn run_contract_file(name: &str, cases: &[ContractCase]) -> Vec<CaseResult> {
    let mut results = Vec::new();
    for case in cases {
        if let Some(report) = compare_with_reference_shells(case) {
            // Investigation aid only; visible with --nocapture.
            println!("{report}");
        }
        results.push(run_case(case));
    }
    let failures: Vec<&CaseResult> = results.iter().filter(|r| r.is_suite_failure()).collect();
    assert!(
        failures.is_empty(),
        "contract file {name} has {} failing case(s):\n{}",
        failures.len(),
        render_results(&results)
    );
    results
}

macro_rules! contract_test {
    ($name:ident, $file:literal) => {
        #[test]
        fn $name() {
            let all = load_all_cases();
            let cases = all
                .get($file)
                .unwrap_or_else(|| panic!("spec file {} not found", $file));
            run_contract_file($file, cases);
        }
    };
}

contract_test!(contract_simple_command, "simple-command.toml");
contract_test!(contract_list_operators, "list-operators.toml");
contract_test!(contract_pipeline, "pipeline.toml");
contract_test!(contract_signal_status, "signal-status.toml");
contract_test!(contract_assignment, "assignment.toml");
contract_test!(contract_redirection, "redirection.toml");
contract_test!(contract_expansion, "expansion.toml");
contract_test!(contract_substitution, "substitution.toml");
contract_test!(contract_process_substitution, "process-substitution.toml");
contract_test!(contract_background, "background.toml");
contract_test!(contract_smart_pipe, "smart-pipe.toml");
contract_test!(contract_interactive, "interactive.toml");

/// Structural gates over the whole suite: unique IDs and the exact-match
/// XFAIL allowlist. A new hidden `xfail` or a stale entry fails here.
#[test]
fn contract_suite_structure() {
    let all = load_all_cases();
    assert!(
        all.values().any(|cases| !cases.is_empty()),
        "no contract cases loaded"
    );
    if let Err(err) = check_duplicate_ids(&all) {
        panic!("duplicate contract case ids:\n{err}");
    }
    let allowlist_path = spec_dir().join("xfail-allowlist.txt");
    let allowlist =
        load_allowlist(&allowlist_path).unwrap_or_else(|err| panic!("allowlist: {err}"));
    if let Err(err) = check_allowlist(&all, &allowlist) {
        panic!("xfail allowlist mismatch:\n{err}");
    }
    let total: usize = all.values().map(Vec::len).sum();
    println!(
        "loaded {total} contract cases from {} spec files",
        all.len()
    );
}
