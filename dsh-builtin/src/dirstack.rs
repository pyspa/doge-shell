//! `pushd` / `popd` / `dirs` — the directory stack.
//!
//! The stack is bash-shaped: slot 0 is always the current directory, so plain
//! `cd` replaces the top without disturbing what is underneath. The shell keeps
//! slot 0 in sync inside `ShellProxy::changepwd`, which every navigation path
//! (`cd`, `z`, bookmarks, and the commands here) funnels through.
//!
//! One deliberate divergence from bash: `+N` and `-N` mean the same thing —
//! "entry N as printed by `dirs -v`". bash counts `-N` from the other end, and
//! having two directions in play is a daily papercut for no real gain.
//!
//! The module is named `dirstack` rather than `dirs` because this crate already
//! depends on the `dirs` crate for `home_dir()`.

use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use std::path::Path;

pub fn pushd_description() -> &'static str {
    "Push a directory onto the directory stack and change to it"
}

pub fn popd_description() -> &'static str {
    "Pop a directory off the directory stack and change to it"
}

pub fn dirs_description() -> &'static str {
    "Show the directory stack"
}

const PUSHD_USAGE: &str = "Usage: pushd [<dir> | +N | -N]";
const POPD_USAGE: &str = "Usage: popd [+N | -N]";
const DIRS_USAGE: &str = "Usage: dirs [-v] [-p] [-l] [-c]";

/// Reads the stack, falling back to `$PWD` when the shell has not changed
/// directory yet (the stack only materialises on the first `changepwd`).
fn stack_or_cwd(proxy: &dyn ShellProxy) -> Vec<String> {
    let stack = proxy.dir_stack();
    if !stack.is_empty() {
        return stack;
    }
    match std::env::current_dir() {
        Ok(dir) => vec![dir.to_string_lossy().into_owned()],
        Err(_) => Vec::new(),
    }
}

/// Where the shell actually is right now, as a canonical path.
fn actual_cwd(proxy: &dyn ShellProxy) -> Option<String> {
    let cwd = proxy.get_current_dir().ok()?;
    Some(
        cwd.canonicalize()
            .unwrap_or(cwd)
            .to_string_lossy()
            .into_owned(),
    )
}

fn same_dir(a: &str, b: &str) -> bool {
    let canonical = |p: &str| {
        Path::new(p)
            .canonicalize()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.to_string())
    };
    a == b || canonical(a) == canonical(b)
}

/// Moves to `next[0]` and installs `next` as the new stack.
///
/// `changepwd` chdirs *before* it can fail: a `*on-chdir-hooks*` entry or
/// direnv can error afterwards, and by then the shell has already moved. So the
/// stack is reconciled against the real cwd rather than against the returned
/// `Result` — treating an error as "nothing happened" would leave `dirs`
/// describing a directory we are no longer in.
///
/// A hook failure is still reported, but only after the stack is consistent.
fn apply(proxy: &mut dyn ShellProxy, mut next: Vec<String>) -> Result<String, String> {
    let head = match next.first() {
        Some(head) => head.clone(),
        None => return Err("directory stack empty".to_string()),
    };

    let outcome = proxy.changepwd(&head);
    let landed = actual_cwd(proxy);

    // The chdir itself failed: we are where we started, so leave the stack be.
    if !landed.as_deref().is_some_and(|cwd| same_dir(cwd, &head)) {
        return Err(match outcome {
            Err(err) => format!("{err}: {head}"),
            Ok(()) => format!("{head}: could not change directory"),
        });
    }

    if let Some(cwd) = landed {
        next[0] = cwd;
    }
    let head = next[0].clone();
    proxy.dir_stack_set(next);

    match outcome {
        Ok(()) => Ok(head),
        // We did move, and the stack now says so; surface the hook error.
        Err(err) => Err(err.to_string()),
    }
}

/// `+N` / `-N` both address entry N of `dirs -v`. A bare `-` is not an index
/// (that is `cd -`, i.e. `$OLDPWD`), so it returns `None`.
pub(crate) fn parse_stack_index(arg: &str) -> Option<usize> {
    let digits = arg.strip_prefix('+').or_else(|| arg.strip_prefix('-'))?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Rotates the stack so entry `index` becomes the current directory, then
/// changes to it. Shared by `pushd +N` and `cd -N`.
pub(crate) fn goto_stack_index(proxy: &mut dyn ShellProxy, index: usize) -> Result<String, String> {
    let mut stack = stack_or_cwd(proxy);
    if index >= stack.len() {
        return Err(format!("{index}: directory stack index out of range"));
    }
    stack.rotate_left(index);
    apply(proxy, stack)
}

pub fn pushd_command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let result = match argv.get(1).map(|s| s.as_str()) {
        Some("-h") | Some("--help") => {
            ctx.write_stdout(PUSHD_USAGE).ok();
            return ExitStatus::ExitedWith(0);
        }
        // `pushd` with no argument swaps the top two entries.
        None => {
            let stack = stack_or_cwd(proxy);
            if stack.len() < 2 {
                Err("no other directory".to_string())
            } else {
                goto_stack_index(proxy, 1)
            }
        }
        Some(arg) => match parse_stack_index(arg) {
            Some(index) => goto_stack_index(proxy, index),
            None => push_directory(arg, proxy),
        },
    };

    match result {
        Ok(_) => print_stack(ctx, proxy, DirsFormat::default()),
        Err(err) => {
            ctx.write_stderr(&format!("pushd: {err}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// `pushd <dir>`: change to `dir`, then slot the directory we came from in
/// underneath it.
fn push_directory(arg: &str, proxy: &mut dyn ShellProxy) -> Result<String, String> {
    let previous = stack_or_cwd(proxy).first().cloned();
    let target = resolve_dir(arg)?;

    // As in `apply`: `changepwd` moves the shell before a chpwd hook can fail,
    // so the outcome is decided by where we actually ended up. Bailing on Err
    // here would skip the insert below and leave `popd` with no way back.
    let outcome = proxy.changepwd(&target);
    let landed = actual_cwd(proxy);

    if !landed.as_deref().is_some_and(|cwd| same_dir(cwd, &target)) {
        return Err(match outcome {
            Err(err) => format!("{err}: {target}"),
            Ok(()) => format!("{target}: could not change directory"),
        });
    }

    // `changepwd` overwrote slot 0 with where we landed; re-insert the old
    // directory below it.
    let mut stack = proxy.dir_stack();
    if stack.is_empty() {
        stack.push(landed.clone().unwrap_or_else(|| target.clone()));
    }
    if let Some(previous) = previous {
        stack.insert(1, previous);
    }
    let head = stack[0].clone();
    proxy.dir_stack_set(stack);

    match outcome {
        Ok(()) => Ok(head),
        Err(err) => Err(err.to_string()),
    }
}

fn resolve_dir(arg: &str) -> Result<String, String> {
    if arg.starts_with('/') {
        return Ok(arg.to_string());
    }
    if arg.starts_with('~') {
        return Ok(shellexpand::tilde(arg).to_string());
    }
    let current = std::env::current_dir().map_err(|err| format!("{err}"))?;
    Path::new(&current)
        .join(arg)
        .canonicalize()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|err| format!("{err}: {arg}"))
}

pub fn popd_command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let index = match argv.get(1).map(|s| s.as_str()) {
        Some("-h") | Some("--help") => {
            ctx.write_stdout(POPD_USAGE).ok();
            return ExitStatus::ExitedWith(0);
        }
        None => 0,
        Some(arg) => match parse_stack_index(arg) {
            Some(index) => index,
            None => {
                ctx.write_stderr(&format!("popd: {arg}: invalid argument"))
                    .ok();
                ctx.write_stderr(POPD_USAGE).ok();
                return ExitStatus::ExitedWith(1);
            }
        },
    };

    let mut stack = stack_or_cwd(proxy);
    if index >= stack.len() {
        ctx.write_stderr(&format!(
            "popd: {index}: directory stack index out of range"
        ))
        .ok();
        return ExitStatus::ExitedWith(1);
    }
    if stack.len() < 2 {
        ctx.write_stderr("popd: directory stack empty").ok();
        return ExitStatus::ExitedWith(1);
    }

    stack.remove(index);

    // Removing anything other than the top leaves us where we are; only
    // dropping slot 0 actually moves the shell.
    if index == 0 {
        if let Err(err) = apply(proxy, stack) {
            ctx.write_stderr(&format!("popd: {err}")).ok();
            return ExitStatus::ExitedWith(1);
        }
    } else {
        proxy.dir_stack_set(stack);
    }

    print_stack(ctx, proxy, DirsFormat::default())
}

#[derive(Clone, Copy, Default)]
struct DirsFormat {
    verbose: bool,
    one_per_line: bool,
    /// Print full paths instead of shortening `$HOME` to `~`.
    long: bool,
}

pub fn dirs_command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let mut format = DirsFormat::default();

    for arg in argv.iter().skip(1) {
        match arg.as_str() {
            "-v" => format.verbose = true,
            "-p" => format.one_per_line = true,
            "-l" => format.long = true,
            "-c" => {
                // Clearing keeps slot 0: the current directory is always on the
                // stack, it just has nothing under it any more.
                let stack = stack_or_cwd(proxy);
                proxy.dir_stack_set(stack.into_iter().take(1).collect());
                return ExitStatus::ExitedWith(0);
            }
            "-h" | "--help" => {
                ctx.write_stdout(DIRS_USAGE).ok();
                return ExitStatus::ExitedWith(0);
            }
            other => {
                ctx.write_stderr(&format!("dirs: {other}: invalid option"))
                    .ok();
                ctx.write_stderr(DIRS_USAGE).ok();
                return ExitStatus::ExitedWith(1);
            }
        }
    }

    print_stack(ctx, proxy, format)
}

fn print_stack(ctx: &Context, proxy: &mut dyn ShellProxy, format: DirsFormat) -> ExitStatus {
    let stack = stack_or_cwd(proxy);
    let home = if format.long {
        None
    } else {
        dirs::home_dir().map(|path| path.to_string_lossy().into_owned())
    };

    let entries: Vec<String> = stack
        .iter()
        .map(|path| shorten(path, home.as_deref()))
        .collect();

    if format.verbose {
        for (index, entry) in entries.iter().enumerate() {
            ctx.write_stdout(&format!("{index:>2}  {entry}")).ok();
        }
    } else if format.one_per_line {
        for entry in &entries {
            ctx.write_stdout(entry).ok();
        }
    } else {
        ctx.write_stdout(&entries.join(" ")).ok();
    }

    ExitStatus::ExitedWith(0)
}

/// Replaces a leading `$HOME` with `~`. Only whole path components match, so
/// `/home/alice-backup` is not shortened for `/home/alice`.
fn shorten(path: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|home| !home.is_empty()) else {
        return path.to_string();
    };
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal proxy modelling how the real `changepwd` behaves: it chdirs and
    /// writes the new directory into slot 0, and *only then* can fail in a
    /// chpwd hook.
    struct StackProxy {
        stack: Vec<String>,
        cwd: String,
        /// The chdir itself fails; nothing moves.
        chdir_fails: bool,
        /// The chdir succeeds and a chpwd hook fails afterwards.
        hook_fails: bool,
    }

    impl StackProxy {
        fn new(stack: &[&str]) -> Self {
            Self {
                stack: stack.iter().map(|s| s.to_string()).collect(),
                cwd: stack.first().unwrap_or(&"/").to_string(),
                chdir_fails: false,
                hook_fails: false,
            }
        }
    }

    impl ShellProxy for StackProxy {
        fn get_current_dir(&self) -> anyhow::Result<std::path::PathBuf> {
            Ok(std::path::PathBuf::from(&self.cwd))
        }
        fn exit_shell(&mut self) {}
        fn dispatch(
            &mut self,
            _ctx: &Context,
            _cmd: &str,
            _argv: Vec<String>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        fn save_path_history(&mut self, _path: &str) {}
        fn changepwd(&mut self, path: &str) -> anyhow::Result<()> {
            if self.chdir_fails {
                anyhow::bail!("No such file or directory");
            }
            // Order matters: the real implementation moves and rewrites slot 0
            // before the hooks run, so a hook failure leaves both changed.
            self.cwd = path.to_string();
            if self.stack.is_empty() {
                self.stack.push(path.to_string());
            } else {
                self.stack[0] = path.to_string();
            }
            if self.hook_fails {
                anyhow::bail!("chpwd hook failed");
            }
            Ok(())
        }
        fn dir_stack(&self) -> Vec<String> {
            self.stack.clone()
        }
        fn dir_stack_set(&mut self, stack: Vec<String>) {
            self.stack = stack;
        }
        fn insert_path(&mut self, _index: usize, _path: &str) {}
        fn get_var(&mut self, _key: &str) -> Option<String> {
            None
        }
        fn set_var(&mut self, _key: String, _value: String) {}
        fn set_env_var(&mut self, _key: String, _value: String) {}
        fn unset_env_var(&mut self, _key: &str) {}
        fn get_alias(&mut self, _name: &str) -> Option<String> {
            None
        }
        fn set_alias(&mut self, _name: String, _command: String) {}
        fn list_aliases(&mut self) -> std::collections::HashMap<String, String> {
            std::collections::HashMap::new()
        }
        fn add_abbr(&mut self, _name: String, _expansion: String) {}
        fn remove_abbr(&mut self, _name: &str) -> bool {
            false
        }
        fn list_abbrs(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn get_abbr(&self, _name: &str) -> Option<String> {
            None
        }
        fn list_mcp_servers(&mut self) -> Vec<dsh_types::mcp::McpServerConfig> {
            Vec::new()
        }
        fn list_execute_allowlist(&mut self) -> Vec<String> {
            Vec::new()
        }
        fn list_exported_vars(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn export_var(&mut self, _key: &str) -> bool {
            true
        }
        fn set_and_export_var(&mut self, _key: String, _value: String) {}
        fn get_github_status(&self) -> (usize, usize, usize) {
            (0, 0, 0)
        }
        fn get_git_branch(&self) -> Option<String> {
            None
        }
        fn get_job_count(&self) -> usize {
            0
        }
        fn get_lisp_var(&self, _key: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn parses_both_index_signs_the_same_way() {
        assert_eq!(parse_stack_index("+2"), Some(2));
        assert_eq!(parse_stack_index("-2"), Some(2));
        assert_eq!(parse_stack_index("+0"), Some(0));
    }

    #[test]
    fn bare_dash_is_not_an_index() {
        // `cd -` must keep meaning $OLDPWD.
        assert_eq!(parse_stack_index("-"), None);
        assert_eq!(parse_stack_index("+"), None);
        assert_eq!(parse_stack_index("-2a"), None);
        assert_eq!(parse_stack_index("foo"), None);
    }

    #[test]
    fn goto_rotates_so_the_chosen_entry_is_on_top() {
        let mut proxy = StackProxy::new(&["/a", "/b", "/c"]);
        assert_eq!(goto_stack_index(&mut proxy, 2), Ok("/c".to_string()));
        assert_eq!(proxy.stack, vec!["/c", "/a", "/b"]);
    }

    #[test]
    fn goto_rejects_out_of_range_index() {
        let mut proxy = StackProxy::new(&["/a", "/b"]);
        assert!(goto_stack_index(&mut proxy, 5).is_err());
        assert_eq!(proxy.stack, vec!["/a", "/b"]);
    }

    #[test]
    fn failed_chdir_leaves_the_stack_untouched() {
        let mut proxy = StackProxy::new(&["/a", "/b", "/c"]);
        proxy.chdir_fails = true;
        assert!(goto_stack_index(&mut proxy, 1).is_err());
        assert_eq!(proxy.stack, vec!["/a", "/b", "/c"]);
        assert_eq!(proxy.cwd, "/a");
    }

    /// `changepwd` moves the shell before a chpwd hook can fail. Treating that
    /// error as "nothing happened" used to leave the stack describing a
    /// directory we had already left, losing entries and duplicating others.
    #[test]
    fn a_failing_chpwd_hook_still_leaves_a_consistent_stack() {
        let mut proxy = StackProxy::new(&["/a", "/b", "/c"]);
        proxy.hook_fails = true;

        // The error is still reported...
        assert!(goto_stack_index(&mut proxy, 2).is_err());
        // ...but we did move, so the stack has to say so.
        assert_eq!(proxy.cwd, "/c");
        assert_eq!(proxy.stack, vec!["/c", "/a", "/b"]);
    }

    #[test]
    fn push_directory_slots_the_previous_directory_underneath() {
        let mut proxy = StackProxy::new(&["/a", "/b"]);
        assert_eq!(push_directory("/tmp", &mut proxy), Ok("/tmp".to_string()));
        assert_eq!(proxy.stack, vec!["/tmp", "/a", "/b"]);
    }

    #[test]
    fn push_directory_reports_failure_without_mutating() {
        let mut proxy = StackProxy::new(&["/a"]);
        proxy.chdir_fails = true;
        assert!(push_directory("/tmp", &mut proxy).is_err());
        assert_eq!(proxy.stack, vec!["/a"]);
        assert_eq!(proxy.cwd, "/a");
    }

    /// Same reasoning as `apply`: if the hook fails after the move, `popd` must
    /// still be able to get back.
    #[test]
    fn push_directory_keeps_the_way_back_when_a_hook_fails() {
        let mut proxy = StackProxy::new(&["/a", "/b"]);
        proxy.hook_fails = true;

        assert!(push_directory("/tmp", &mut proxy).is_err());
        assert_eq!(proxy.cwd, "/tmp");
        assert_eq!(proxy.stack, vec!["/tmp", "/a", "/b"]);
    }

    #[test]
    fn shorten_only_matches_whole_components() {
        assert_eq!(shorten("/home/alice", Some("/home/alice")), "~");
        assert_eq!(shorten("/home/alice/src", Some("/home/alice")), "~/src");
        assert_eq!(
            shorten("/home/alice-backup", Some("/home/alice")),
            "/home/alice-backup"
        );
        assert_eq!(shorten("/etc", Some("/home/alice")), "/etc");
        assert_eq!(shorten("/etc", None), "/etc");
    }
}
