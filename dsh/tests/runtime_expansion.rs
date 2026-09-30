//! Per-job runtime word expansion: values resolve when the job is selected,
//! not when the line is parsed.

mod common;

use common::{false_path, run_command, run_interactive, true_path};

fn stdout_of(command: &str) -> String {
    String::from_utf8_lossy(&run_command(command).stdout).to_string()
}

#[test]
fn same_line_variable_timing() {
    let out = stdout_of("FOO=runtime; echo $FOO");
    assert!(
        out.lines().any(|line| line.trim() == "runtime"),
        "same-line variable must resolve at materialization, got {out:?}"
    );
}

#[test]
fn same_line_exit_status_false() {
    let out = stdout_of(&format!("{}; echo $?", false_path()));
    assert!(
        out.lines().any(|line| line.trim() == "1"),
        "false; echo $? must report 1, got {out:?}"
    );
}

#[test]
fn same_line_exit_status_true() {
    let out = stdout_of(&format!("{}; echo $?", true_path()));
    assert!(
        out.lines().any(|line| line.trim() == "0"),
        "true; echo $? must report 0, got {out:?}"
    );
}

#[test]
fn assignment_only_job_publishes_zero_status() {
    let out = stdout_of(&format!("{}; FOO=dyn_status_probe; echo $?", false_path()));
    assert!(
        out.lines().any(|line| line.trim() == "0"),
        "a successful assignment must clear the previous failure, got {out:?}"
    );
}

#[test]
fn cwd_sensitive_glob() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(root.path().join("outside.txt"), "x").unwrap();
    let target = root.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("inside-a.txt"), "a").unwrap();
    std::fs::write(target.join("inside-b.txt"), "b").unwrap();

    let out = stdout_of(&format!(
        "cd {}; /usr/bin/printf '[%s]\\n' *.txt",
        target.display()
    ));
    assert!(
        out.contains("inside-a.txt") && out.contains("inside-b.txt"),
        "glob must run in changed cwd, got {out:?}"
    );
    assert!(
        !out.contains("outside.txt"),
        "glob must not see parse-time cwd, got {out:?}"
    );
}

#[test]
fn tilde_sees_same_line_home() {
    let home = tempfile::tempdir().expect("tempdir");
    let home_str = home.path().to_string_lossy().to_string();
    let out = stdout_of(&format!("HOME={home_str}; echo ~"));
    assert!(
        out.lines().any(|line| line.trim() == home_str),
        "tilde must see same-line HOME, got {out:?}"
    );
}

#[test]
fn assignment_value_timing() {
    let out = stdout_of("A=first; B=$A; echo $B");
    assert!(
        out.lines().any(|line| line.trim() == "first"),
        "assignment value must see prior job state, got {out:?}"
    );
}

#[test]
fn redirect_target_timing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("result.txt");
    let target_str = target.to_string_lossy().to_string();
    let out = run_command(&format!("TARGET={target_str}; printf hi > $TARGET"));
    assert!(out.status.success(), "redirect line failed: {out:?}");
    let body = std::fs::read_to_string(&target).expect("redirect target file");
    assert_eq!(body, "hi");
    assert!(
        !dir.path().join("$TARGET").exists(),
        "literal $TARGET file must not be created"
    );
}

#[test]
fn alias_body_remains_dynamic() {
    let home = tempfile::tempdir().expect("tempdir");
    let home_str = home.path().to_string_lossy().to_string();
    let output = run_interactive(&[
        "alias runtime_probe='echo $HOME'",
        &format!("HOME={home_str}; runtime_probe"),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.lines().any(|line| line.trim() == home_str),
        "alias body $HOME must resolve at use time, got {stdout:?}"
    );
}

#[test]
fn mixed_double_quoted_substitution_is_one_argv() {
    let out = stdout_of("/usr/bin/printf '[%s]\\n' \"a $(/usr/bin/printf 'x y') b\"");
    assert_eq!(
        out.trim(),
        "[a x y b]",
        "mixed quoted substitution broke: {out:?}"
    );
}

#[test]
fn unquoted_substitution_composition() {
    let out = stdout_of("/usr/bin/printf '[%s]\\n' pre$(/usr/bin/printf 'a b')post");
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(lines, vec!["[prea]", "[bpost]"], "got {out:?}");
}

#[test]
fn quoted_substitution_preserves_whitespace() {
    let out = stdout_of("/usr/bin/printf '[%s]\\n' \"$(/usr/bin/printf 'a b')\"");
    assert_eq!(out.trim(), "[a b]", "got {out:?}");
}

#[test]
fn empty_quotes_are_one_empty_argv() {
    let out = stdout_of("/usr/bin/printf '<%s>\\n' \"\"");
    assert_eq!(out.trim(), "<>", "double empty broke: {out:?}");
    let out = stdout_of("/usr/bin/printf '<%s>\\n' ''");
    assert_eq!(out.trim(), "<>", "single empty broke: {out:?}");
}

#[test]
fn protected_glob_behavior() {
    assert_eq!(stdout_of("echo \"*.toml\"").trim(), "*.toml");
    assert_eq!(stdout_of("echo \"*\"nomatch*").trim(), "*nomatch*");
    assert_eq!(stdout_of("echo \\*").trim(), "*");
}

#[test]
fn default_ifs_splits_unquoted_variable() {
    let out = stdout_of("unset IFS; X='a b'; /usr/bin/printf '[%s]\\n' $X");
    assert_eq!(out.trim(), "[a]\n[b]", "got {out:?}");
}

#[test]
fn quoted_variable_stays_one_field() {
    let out = stdout_of("X='a b'; /usr/bin/printf '[%s]\\n' \"$X\"");
    assert_eq!(out.trim(), "[a b]", "got {out:?}");
}

#[test]
fn set_empty_unquoted_contributes_zero_fields() {
    let out = stdout_of("EMPTY=; /usr/bin/printf '[%s]\\n' before $EMPTY after");
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(lines, vec!["[before]", "[after]"], "got {out:?}");
}

#[test]
fn unset_unquoted_contributes_zero_fields() {
    let out = stdout_of("/usr/bin/printf '[%s]\\n' before $DOGESH_UNSET_PROBE_XYZ after");
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(lines, vec!["[before]", "[after]"], "got {out:?}");
    assert!(
        !out.contains("DOGESH_UNSET_PROBE_XYZ"),
        "unset must not leak source spelling, got {out:?}"
    );
}

#[test]
fn quoted_empty_keeps_one_empty_argv() {
    let out = stdout_of("EMPTY=; /usr/bin/printf '[%s]\\n' \"$EMPTY\"");
    assert_eq!(out.trim(), "[]", "got {out:?}");
}

#[test]
fn ifs_empty_disables_splitting() {
    let out = stdout_of("IFS=; X='a b'; /usr/bin/printf '[%s]\\n' $X");
    assert_eq!(out.trim(), "[a b]", "got {out:?}");
}

#[test]
fn custom_comma_ifs_keeps_interior_empty() {
    let out = stdout_of("IFS=,; X='a,,b'; /usr/bin/printf '[%s]\\n' $X");
    assert_eq!(out.trim(), "[a]\n[]\n[b]", "got {out:?}");
    let out = stdout_of("IFS=,; X=',a'; /usr/bin/printf '[%s]\\n' $X");
    assert_eq!(out.trim(), "[]\n[a]", "got {out:?}");
    let out = stdout_of("IFS=,; X='a,'; /usr/bin/printf '[%s]\\n' $X");
    assert_eq!(out.trim(), "[a]", "got {out:?}");
}

#[test]
fn mixed_ifs_whitespace_and_delimiter() {
    let out = stdout_of("IFS=': '; X='a: :b'; /usr/bin/printf '[%s]\\n' $X");
    assert_eq!(out.trim(), "[a]\n[]\n[b]", "got {out:?}");
}

#[test]
fn cross_wordpart_leading_delimiter() {
    let out = stdout_of("X=' a'; /usr/bin/printf '[%s]\\n' pre$X");
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(lines, vec!["[pre]", "[a]"], "got {out:?}");
}

#[test]
fn cross_wordpart_trailing_delimiter() {
    let out = stdout_of("X='a '; /usr/bin/printf '[%s]\\n' ${X}post");
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(lines, vec!["[a]", "[post]"], "got {out:?}");
}

#[test]
fn variable_generated_glob_expands_unquoted_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b").unwrap();
    let out = stdout_of(&format!(
        "cd {}; PAT='*.txt'; /usr/bin/printf '[%s]\\n' $PAT",
        dir.path().display()
    ));
    assert!(
        out.contains("a.txt") && out.contains("b.txt"),
        "unquoted PAT must glob, got {out:?}"
    );
    let out = stdout_of(&format!(
        "cd {}; PAT='*.txt'; /usr/bin/printf '[%s]\\n' \"$PAT\"",
        dir.path().display()
    ));
    assert_eq!(
        out.trim(),
        "[*.txt]",
        "quoted PAT must stay literal, got {out:?}"
    );
}

#[test]
fn variable_generated_braces_do_not_expand() {
    let out = stdout_of("X='{a,b}'; /usr/bin/printf '[%s]\\n' $X");
    assert_eq!(out.trim(), "[{a,b}]", "got {out:?}");
}

#[test]
fn command_substitution_respects_custom_ifs() {
    let out = stdout_of("IFS=:; /usr/bin/printf '[%s]\\n' $(/usr/bin/printf 'a:b')");
    assert_eq!(out.trim(), "[a]\n[b]", "got {out:?}");
    let out = stdout_of("IFS=,; /usr/bin/printf '[%s]\\n' $(/usr/bin/printf 'a b')");
    assert_eq!(out.trim(), "[a b]", "got {out:?}");
}

#[test]
fn command_substitution_trailing_newlines_removed_before_split() {
    let out = stdout_of("IFS=,; /usr/bin/printf '[%s]\\n' $(/usr/bin/printf 'a\\n')");
    assert_eq!(out.trim(), "[a]", "got {out:?}");
}

#[test]
fn assignment_rhs_does_not_split() {
    let out = stdout_of("IFS=:; X='a:b'; Y=$X; /usr/bin/printf '%s\\n' \"$Y\"");
    assert_eq!(out.trim(), "a:b", "got {out:?}");
}

#[test]
fn redirect_target_with_spaces_stays_one_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = run_command(&format!(
        "cd {}; TARGET='file with spaces.txt'; echo hello > $TARGET",
        dir.path().display()
    ));
    assert!(out.status.success(), "redirect line failed: {out:?}");
    let body =
        std::fs::read_to_string(dir.path().join("file with spaces.txt")).expect("spaced target");
    assert_eq!(body.trim(), "hello");
}

#[test]
fn glob_uses_changed_cwd_inside_substitution() {
    let root = tempfile::tempdir().expect("tempdir");
    let target = root.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("inner-a.txt"), "a").unwrap();
    std::fs::write(target.join("inner-b.txt"), "b").unwrap();
    let out = stdout_of(&format!(
        "cd {}; echo $(/usr/bin/printf '%s\\n' *)",
        target.display()
    ));
    assert!(
        out.contains("inner-a.txt") && out.contains("inner-b.txt"),
        "inner glob must see changed cwd, got {out:?}"
    );
}

#[test]
fn ambiguous_redirect_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b").unwrap();
    let before_a = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
    let before_b = std::fs::read_to_string(dir.path().join("b.txt")).unwrap();
    let out = run_command(&format!("cd {}; echo hi > *.txt", dir.path().display()));
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.to_lowercase().contains("ambiguous"),
        "expected ambiguous redirect, got stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        stderr
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        before_a
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
        before_b
    );
}
