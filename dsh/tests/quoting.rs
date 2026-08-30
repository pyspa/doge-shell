//! Word structure: adjacent parts form one argument, double quotes
//! interpolate, single quotes do not.
//!
//! Before this, `echo "a $HOME b"` printed an empty line -- the grammar could
//! not match a `$` inside double quotes, so the argument was dropped without a
//! word of explanation. These tests pin the fix from the user's side.

mod common;

use common::run_command;

fn stdout_of(command: &str) -> String {
    String::from_utf8_lossy(&run_command(command).stdout)
        .trim()
        .to_string()
}

#[test]
fn double_quotes_interpolate_variables() {
    assert_eq!(stdout_of("echo \"a $HOME b\""), format!("a {} b", home()));
    assert_eq!(stdout_of("echo \"$HOME\""), home());
    assert_eq!(stdout_of("echo \"${HOME}\""), home());
}

#[test]
fn single_quotes_stay_literal() {
    assert_eq!(stdout_of("echo '$HOME'"), "$HOME");
    assert_eq!(stdout_of("echo 'a $HOME b'"), "a $HOME b");
}

#[test]
fn a_dollar_that_starts_no_variable_stays_literal() {
    assert_eq!(stdout_of("echo \"price is $\""), "price is $");
}

#[test]
fn double_quotes_honour_only_the_escapes_shells_honour() {
    // `\$` collapses; `\p` is not an escape and survives as typed, so a Windows
    // path does not quietly lose its separators.
    assert_eq!(stdout_of(r#"echo "escaped \$HOME""#), "escaped $HOME");
    assert_eq!(stdout_of(r#"echo "C:\path\to""#), r"C:\path\to");
}

#[test]
fn adjacent_parts_form_one_argument() {
    let home = home();
    assert_eq!(stdout_of("echo $HOME/x"), format!("{home}/x"));
    assert_eq!(stdout_of("echo --file=$HOME/x"), format!("--file={home}/x"));
    // One argument, not two. `printf` repeats its format per argument, so a
    // split would show up as two bracketed lines instead of one.
    assert_eq!(
        stdout_of("/usr/bin/printf '[%s]\n' --file=$HOME/x"),
        format!("[--file={home}/x]")
    );
}

/// `]` used to end the parse, so `echo a]b` printed `a` and dropped the rest.
#[test]
fn a_closing_bracket_is_an_ordinary_word_character() {
    assert_eq!(stdout_of("echo a]b"), "a]b");
}

/// A bare word must never be read as a variable name. `echo $USER LANG` used to
/// print the value of `LANG`, but only on lines that happened to trigger the
/// expansion pass -- silent, and dependent on unrelated text in the line.
#[test]
fn a_bare_word_is_not_a_variable_reference() {
    assert_eq!(stdout_of("echo $HOME LANG"), format!("{} LANG", home()));
}

fn home() -> String {
    std::env::var("HOME").expect("HOME is set in the test environment")
}

/// A span is one argument whether or not the line happens to trigger the
/// expansion pass. Adjacent parts used to be pushed separately, so `a"b"c`
/// arrived as three arguments.
#[test]
fn adjacent_quoted_parts_join_without_expansion() {
    assert_eq!(stdout_of(r#"echo a"b"c"#), "abc");
    assert_eq!(stdout_of("echo 'a'b"), "ab");
    assert_eq!(stdout_of(r#"echo "x y"z"#), "x yz");
}

/// Escapes are collapsed before the value is handed on, on both paths. The
/// expansion pass used to take the raw text, so a backslash survived into argv
/// whenever the line also contained a variable or a glob.
#[test]
fn escapes_are_collapsed_on_both_paths() {
    assert_eq!(stdout_of(r"echo a\ b"), "a b");
    assert_eq!(stdout_of(r"echo a\ b $HOME"), format!("a b {}", home()));
    assert_eq!(stdout_of(r"echo \* $HOME"), format!("* {}", home()));
}

/// A tilde is special only at the start of a word, as in every other shell.
#[test]
fn tilde_expands_only_at_the_start_of_a_word() {
    assert_eq!(stdout_of("echo ~"), home());
    assert_eq!(stdout_of("echo ~/x"), format!("{}/x", home()));
    assert_eq!(stdout_of(r#"echo "x"~/y"#), "x~/y");
}

/// `$?` carries the previous line's status.
///
/// It is resolved when the line is expanded, which happens before the first
/// job of that line runs -- so `false; echo $?` reports the status of the
/// *previous* line, not of `false`. Pinned here so the limitation is explicit;
/// per-command expansion is what would close it.
#[test]
fn dollar_question_reports_the_previous_lines_status() {
    let output = common::run_interactive(&["false", "echo $?", "true", "echo $?"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let seen: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| *line == "0" || *line == "1")
        .collect();

    assert_eq!(seen, vec!["1", "0"], "got stdout: {stdout:?}");
}

#[test]
fn dollar_dollar_is_the_shell_pid() {
    let pid = stdout_of("echo $$");
    assert!(pid.parse::<u32>().is_ok(), "expected a pid, got {pid:?}");
}

/// A metacharacter that came from a quote is a literal, even when another part
/// of the same word is a real pattern. The glob flag is per word, so `"*"x*`
/// used to match files against the quoted asterisk too.
#[test]
fn a_quoted_metacharacter_is_not_a_pattern() {
    assert_eq!(stdout_of(r#"echo "*".toml"#), "*.toml");
    assert_eq!(stdout_of(r#"echo "*"nomatch*"#), "*nomatch*");
}
