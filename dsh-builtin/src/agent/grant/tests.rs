use super::*;

fn grant() -> TaskGrant {
    TaskGrant::default()
}

#[test]
fn an_unrelated_option_is_left_for_the_caller() {
    let mut g = grant();
    assert!(!apply_grant_option(&mut g, "--tokens", "100").unwrap());
    assert_eq!(g, TaskGrant::default());
}

// macOS-only ignore: `/tmp` is a symlink to `/private/tmp` there, so the
// granted root canonicalizes and the `/tmp` expectation below fails.
// Linux keeps `/tmp` as-is.
#[cfg_attr(target_os = "macos", ignore)]
#[test]
fn read_and_write_require_an_existing_directory() {
    let mut g = grant();
    assert!(apply_grant_option(&mut g, "--read", "/definitely/not/a/real/path").is_err());
    assert!(apply_grant_option(&mut g, "--read", "/tmp").unwrap());
    assert_eq!(g.read_roots, vec![PathBuf::from("/tmp")]);
}

// macOS-only ignore: same `/tmp` -> `/private/tmp` canonicalization as above.
#[cfg_attr(target_os = "macos", ignore)]
#[test]
fn write_lands_in_the_write_roots_not_the_read_roots() {
    let mut g = grant();
    apply_grant_option(&mut g, "--write", "/tmp").unwrap();
    assert_eq!(g.write_roots, vec![PathBuf::from("/tmp")]);
    assert!(g.read_roots.is_empty());
}

#[test]
fn allow_command_and_allow_mcp_accumulate() {
    let mut g = grant();
    apply_grant_option(&mut g, "--allow-command", "cargo test").unwrap();
    apply_grant_option(&mut g, "--allow-command", "cargo build").unwrap();
    apply_grant_option(&mut g, "--allow-mcp", "mcp:server:tool:{}").unwrap();
    assert_eq!(g.commands, vec!["cargo test", "cargo build"]);
    assert_eq!(g.mcp_calls, vec!["mcp:server:tool:{}"]);
}

/// A CIDR range or a port would grant far more than the one host it looks
/// like at a glance.
#[test]
fn network_must_be_an_exact_host() {
    let mut g = grant();
    for bad in ["10.0.0.0/8", "example.com:443", "*.example.com", "  "] {
        assert!(
            apply_grant_option(&mut g, "--network", bad).is_err(),
            "{bad}"
        );
    }
    assert!(apply_grant_option(&mut g, "--network", "example.com").unwrap());
    assert_eq!(g.network_hosts, vec!["example.com"]);
}

/// Stray leading/trailing whitespace must not survive into the stored host -
/// it passed the `is_empty` check on the trimmed value but used to be pushed
/// untrimmed, silently breaking the exact-match comparison this grant exists
/// to guarantee.
#[test]
fn network_trims_the_host_it_stores() {
    let mut g = grant();
    assert!(apply_grant_option(&mut g, "--network", "  example.com  ").unwrap());
    assert_eq!(g.network_hosts, vec!["example.com"]);
}

#[test]
fn env_accumulates_names() {
    let mut g = grant();
    apply_grant_option(&mut g, "--env", "HOME").unwrap();
    apply_grant_option(&mut g, "--env", "PATH").unwrap();
    assert_eq!(g.environment, vec!["HOME", "PATH"]);
}

/// `--env NAME=value` looks like it sets `value`, but nothing ever reads a
/// `name=value` pair back out of `environment` - the value always comes from
/// the shell's own variable at run time. Rejecting it here is cheaper than
/// letting someone discover it silently granted a variable that matches
/// nothing.
#[test]
fn env_rejects_a_name_value_pair() {
    let mut g = grant();
    assert!(apply_grant_option(&mut g, "--env", "AI_CHAT_HOOKS=off").is_err());
    assert!(g.environment.is_empty());
}
