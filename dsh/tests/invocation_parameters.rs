//! Invocation arguments use the existing deadline/resource harness.
mod common;

use std::time::Duration;

fn invoke(source: &str, args: &[&str]) -> String {
    let mut cli = vec!["-c", source];
    cli.extend_from_slice(args);
    let output = common::run_dsh(cli, Duration::from_secs(15));
    assert!(
        output.status.success(),
        "source: {source}, args: {args:?}, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Existing command-mode terminal housekeeping emits CRLF separators.
    String::from_utf8(output.stdout)
        .unwrap()
        .replace("\r\n", "")
}

#[test]
fn basic_default_custom_name_and_hyphen_arguments() {
    assert_eq!(
        invoke(
            r#"printf "%s:%s:%s:%s\n" "$0" "$1" "$2" "$#""#,
            &["cmd", "a", "b"]
        ),
        "cmd:a:b:2\n"
    );
    assert_eq!(invoke(r#"printf "%s:%s\n" "$0" "$#""#, &[]), "dogesh:0\n");
    assert_eq!(
        invoke(r#"printf "%s:%s\n" "$0" "$#""#, &["custom-name"]),
        "custom-name:0\n"
    );
    for arg in ["-x", "--hello"] {
        assert_eq!(
            invoke(r#"printf "%s\n" "$1""#, &["cmd", arg]),
            format!("{arg}\n")
        );
    }
}

#[test]
fn positions_empty_unset_and_tenth_parameter() {
    assert_eq!(
        invoke(
            r#"printf "<%s>\n" "$1" "$2" "$3" "$#""#,
            &["x", "", "value"]
        ),
        "<>\n<value>\n<>\n<2>\n"
    );
    assert_eq!(
        invoke(
            r#"printf "<%s>\n" "$10" "${10}" "${11}""#,
            &["x", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k"]
        ),
        "<a0>\n<j>\n<k>\n"
    );
    assert_eq!(
        invoke(r#"set 1 shadow; printf "%s\n" "$1""#, &["x", "real"]),
        "real\n"
    );
}

#[test]
fn quoted_at_preserves_spaces_empty_and_glob_spelling() {
    assert_eq!(
        invoke(r#"printf "<%s>\n" "$@""#, &["x", "a b", "", "*.rs"]),
        "<a b>\n<>\n<*.rs>\n"
    );
    assert_eq!(
        invoke(r#"printf "<%s>\n" "pre$@post""#, &["x", "a", "b", "c"]),
        "<prea>\n<b>\n<cpost>\n"
    );
    assert_eq!(
        invoke(r#"printf "<%s>\n" "pre$@post""#, &["x"]),
        "<prepost>\n"
    );
    // Surrounding arguments make the zero-field assertion observable; printf
    // with no value arguments itself prints an empty conversion.
    assert_eq!(
        invoke(r#"printf "<%s>\n" before "$@" after"#, &["x"]),
        "<before>\n<after>\n"
    );
}

#[test]
fn star_joins_with_first_ifs_character_including_utf8_and_null() {
    for (prefix, value) in [
        ("", "a b c"),
        ("IFS=:;", "a:b:c"),
        ("IFS=;", "abc"),
        ("IFS='é:';", "aébéc"),
    ] {
        assert_eq!(
            invoke(
                &format!(r#"{prefix} printf "<%s>\n" "$*""#),
                &["x", "a", "b", "c"]
            ),
            format!("<{value}>\n")
        );
        assert_eq!(
            invoke(
                &format!(r#"{prefix} X="$*"; printf "<%s>\n" "$X""#),
                &["x", "a", "b", "c"]
            ),
            format!("<{value}>\n")
        );
        assert_eq!(
            invoke(
                &format!(r#"{prefix} X="$@"; printf "<%s>\n" "$X""#),
                &["x", "a", "b", "c"]
            ),
            format!("<{value}>\n")
        );
    }
}

#[test]
fn unquoted_at_and_star_split_but_null_ifs_keeps_at_boundaries() {
    assert_eq!(
        invoke(r#"printf "<%s>\n" $@"#, &["x", "a b", "", "c"]),
        "<a>\n<b>\n<c>\n"
    );
    assert_eq!(
        invoke(r#"printf "<%s>\n" $*"#, &["x", "a b", "c"]),
        "<a>\n<b>\n<c>\n"
    );
    assert_eq!(
        invoke(r#"printf "<%s>\n" "$@""#, &["x", "a b", "c"]),
        "<a b>\n<c>\n"
    );
    assert_eq!(
        invoke(r#"IFS=; printf "<%s>\n" $@"#, &["x", "a b", "c"]),
        "<a b>\n<c>\n"
    );
    assert_eq!(
        invoke(r#"IFS=; printf "<%s>\n" $*"#, &["x", "a b", "c"]),
        "<a bc>\n"
    );
}

#[test]
fn invocation_inherits_into_pipeline_and_helpers() {
    assert_eq!(
        invoke(
            r#"printf "<%s>\n" "$1" "$2" | cat; printf "PARENT:%s\n" "$1""#,
            &["cmd", "alpha", "beta"]
        ),
        "<alpha>\n<beta>\nPARENT:alpha\n"
    );
    for source in [
        r#"echo "$(printf "%s" "$1")""#,
        // Parenthesized expansion uses the existing Subshell helper mode.
        r#"echo ( printf "%s" "$1" )"#,
        r#"cat <(echo "$1")"#,
    ] {
        assert_eq!(invoke(source, &["x", "alpha"]), "alpha\n");
    }
}

#[test]
fn scalar_arithmetic_parameter_operands_and_redirects() {
    assert_eq!(
        invoke(
            r#"echo $(( $1 + $# )); echo ${DOGESH_INVOCATION_UNSET:-$1}; echo ${DOGESH_INVOCATION_UNSET:-$#}; echo "${DOGESH_INVOCATION_UNSET:-$*}""#,
            &["x", "3"]
        ),
        "4\n3\n1\n3\n"
    );
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("with space");
    let path = file.to_str().unwrap();
    assert_eq!(
        invoke(
            r#"printf "%s" "$2" > "$1"; cat "$1""#,
            &["x", path, "redirect value"]
        ),
        "redirect value"
    );
}

#[test]
fn positional_values_do_not_become_process_environment_variables() {
    let output = invoke("env", &["x", "secret-invocation-arg"]);
    assert!(!output.contains("secret-invocation-arg"));
    assert!(!output.lines().any(|line| line.starts_with("1=")));
}

#[test]
fn unknown_positional_without_command_is_an_error() {
    let output = common::run_dsh(["foo"], Duration::from_secs(15));
    assert!(!output.status.success());
}

#[test]
fn unquoted_at_performs_pathname_expansion_per_parameter() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("one.rs");
    std::fs::write(&file, "").unwrap();
    let pattern = format!("{}/*.rs", temp.path().display());
    assert_eq!(
        invoke(r#"printf "<%s>\n" $@"#, &["x", &pattern, "tail"]),
        format!("<{}>\n<tail>\n", file.display())
    );
    assert_eq!(
        invoke(r#"printf "<%s>\n" "$@""#, &["x", &pattern]),
        format!("<{pattern}>\n")
    );
}

#[test]
fn option_looking_command_names_are_not_shell_options() {
    for name in ["--help", "-l", "-c", "--notebook"] {
        assert_eq!(
            invoke(r#"printf "%s:%s\n" "$0" "$1""#, &[name, "value"]),
            format!("{name}:value\n")
        );
    }
}
