//! Reading `ai-hooks.json`.
//!
//! Strict on purpose. A hook that silently does not fire is worse than no hook:
//! the person who wrote it believes a check is running. So an unknown field, an
//! unknown event name, a duplicate id and a `command` written as a string are
//! all load errors, and a load error refuses the chat rather than continuing
//! without the hooks.

use serde::{Deserialize, Deserializer, de};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::UNIX_EPOCH;

use crate::ShellProxy;

/// The configuration file, looked up like every other `dsh` config file.
pub(crate) const HOOKS_CONFIG_FILE: &str = "ai-hooks.json";
/// Points at a different file. Resolved shell-variable first, like every other
/// AI setting.
pub(crate) const HOOKS_CONFIG_KEY: &str = "DSH_AI_HOOKS_CONFIG";
/// `0` / `false` / `off` / `no` stops the file from being read at all.
pub(crate) const HOOKS_ENABLED_KEY: &str = "AI_CHAT_HOOKS";
/// Set on every hook process. A hook that starts another `dsh` must not have
/// that shell run hooks of its own.
pub(crate) const HOOK_DEPTH_ENV: &str = "DSH_HOOK_DEPTH";

const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MIN_TIMEOUT_MS: u64 = 100;
const MAX_TIMEOUT_MS: u64 = 60_000;
/// One tool call may wait for at most this many hooks.
const MAX_HOOKS_PER_EVENT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum HookEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    ResponseComplete,
}

impl HookEvent {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session-start",
            HookEvent::UserPromptSubmit => "user-prompt-submit",
            HookEvent::PreToolUse => "pre-tool-use",
            HookEvent::PostToolUse => "post-tool-use",
            HookEvent::ResponseComplete => "response-complete",
        }
    }

    /// Can this event still stop something from happening?
    ///
    /// Decides both whether a `decision` is honoured and what a broken hook
    /// means: a gate that can be bypassed by being slow is not a gate, while an
    /// observer that breaks must not take the shell down with it.
    pub(crate) fn is_gate(self) -> bool {
        matches!(self, HookEvent::UserPromptSubmit | HookEvent::PreToolUse)
    }

    /// Does this event have somewhere to put `additional_context`?
    ///
    /// `session-start` would have to change the system prompt, which decides
    /// whether the previous conversation is carried forward - a hook whose
    /// output varied would then silently end the conversation every turn.
    /// `response-complete` happens after the last thing the model reads.
    pub(crate) fn uses_context(self) -> bool {
        matches!(
            self,
            HookEvent::UserPromptSubmit | HookEvent::PreToolUse | HookEvent::PostToolUse
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HookMatch {
    /// Tool names, with `*` allowed only as the last character (`mcp__*`).
    /// Not a regular expression: configuration is not the place for one.
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HookDefinition {
    pub id: String,
    pub events: Vec<HookEvent>,
    #[serde(default, rename = "match")]
    pub matcher: Option<HookMatch>,
    #[serde(deserialize_with = "deserialize_command")]
    pub command: Vec<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

impl HookDefinition {
    pub(crate) fn timeout_ms(&self) -> u64 {
        self.timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS)
    }

    fn matches_tool(&self, tool: Option<&str>) -> bool {
        let Some(matcher) = &self.matcher else {
            return true;
        };
        if matcher.tools.is_empty() {
            return true;
        }
        let Some(tool) = tool else {
            // A tool matcher on an event that carries no tool cannot be
            // satisfied; firing anyway would be a surprise in the permissive
            // direction.
            return false;
        };
        matcher
            .tools
            .iter()
            .any(|pattern| match_tool(pattern, tool))
    }
}

fn match_tool(pattern: &str, tool: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => tool.starts_with(prefix),
        None => pattern == tool,
    }
}

fn enabled_by_default() -> bool {
    true
}

/// Pin `command[0]` to something a later `chdir` cannot reinterpret.
///
/// The runner sets `current_dir` to the chat's working directory, and on Unix a
/// relative program is resolved *after* that. `["/opt/dsh-hooks/hook.sh"]` therefore ran
/// whatever `./hook.sh` happened to sit in the repository the user had cd'd
/// into - which is exactly the "cloning a repository should not be enough to
/// run its commands" case that keeps `.dsh/hooks.json` unread.
///
/// - absolute path: kept as is
/// - bare name (no `/`): resolved against `PATH` **now**, so the lookup cannot
///   be steered by the directory the hook later runs in. Left alone when it is
///   not on `PATH`; the spawn fails with a clear error and `doctor hooks`
///   already reports it, which beats refusing every chat over a typo.
/// - anything else (`./x`, `../x`, `a/b`): refused
fn normalize_program(program: &str) -> Result<String, String> {
    let path = Path::new(program);
    if path.is_absolute() {
        return Ok(program.to_string());
    }
    if program.contains('/') {
        return Err(format!(
            "`{program}` is a relative path; write an absolute path, or a bare name to look up on PATH. A relative one resolves against whatever directory the chat is in when the hook runs"
        ));
    }

    let Some(paths) = std::env::var_os("PATH") else {
        return Ok(program.to_string());
    };
    Ok(std::env::split_paths(&paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.display().to_string())
        .unwrap_or_else(|| program.to_string()))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HooksFile {
    version: u32,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    #[serde(default)]
    hooks: Vec<HookDefinition>,
}

/// A hook is executed directly, never through a shell, so the configuration has
/// to spell the argument vector out. Accepting a string would promise shell
/// syntax that is not there, and `|` would quietly become an argument.
fn deserialize_command<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::Array(items) => items
            .into_iter()
            .map(|item| match item {
                Value::String(value) => Ok(value),
                other => Err(de::Error::custom(format!(
                    "`command` entries must be strings, found {other}"
                ))),
            })
            .collect(),
        Value::String(_) => Err(de::Error::custom(
            "`command` must be an array of strings, e.g. [\"python3\", \"/path/to/hook.py\"]. A hook runs directly, not through a shell, so put any pipeline in a script file.",
        )),
        other => Err(de::Error::custom(format!(
            "`command` must be an array of strings, found {other}"
        ))),
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LoadedHooks {
    hooks: Vec<HookDefinition>,
}

impl LoadedHooks {
    pub(crate) fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub(crate) fn all(&self) -> &[HookDefinition] {
        &self.hooks
    }

    pub(crate) fn matching(&self, event: HookEvent, tool: Option<&str>) -> Vec<&HookDefinition> {
        self.hooks
            .iter()
            .filter(|hook| hook.enabled && hook.events.contains(&event) && hook.matches_tool(tool))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSignature {
    path: PathBuf,
    modified_ms: u128,
    len: u64,
}

static CACHE: LazyLock<Mutex<Option<(FileSignature, LoadedHooks)>>> =
    LazyLock::new(|| Mutex::new(None));

#[cfg(test)]
pub(crate) fn clear_cache() {
    if let Ok(mut cache) = CACHE.lock() {
        *cache = None;
    }
}

/// Are hooks switched on for this shell?
pub(crate) fn enabled(proxy: &mut dyn ShellProxy) -> bool {
    match super::super::resolve_setting(proxy, HOOKS_ENABLED_KEY) {
        None => true,
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
    }
}

/// Is this shell itself running inside a hook?
///
/// Read from the process environment only. Going through `resolve_setting`
/// would let a shell variable clear the flag, which is exactly the loop this
/// prevents.
pub(crate) fn nested_in_a_hook() -> bool {
    std::env::var_os(HOOK_DEPTH_ENV).is_some()
}

pub(crate) fn config_path(proxy: &mut dyn ShellProxy) -> Option<PathBuf> {
    if let Some(path) = super::super::resolve_setting(proxy, HOOKS_CONFIG_KEY) {
        return Some(PathBuf::from(path));
    }
    crate::config_paths::find_config_file(HOOKS_CONFIG_FILE)
}

/// The hooks this shell should run, or an error that must stop the turn.
pub(crate) fn load(proxy: &mut dyn ShellProxy) -> Result<LoadedHooks, String> {
    if !enabled(proxy) || nested_in_a_hook() {
        return Ok(LoadedHooks::default());
    }

    let Some(path) = config_path(proxy) else {
        return Ok(LoadedHooks::default());
    };
    if !path.is_file() {
        // Only an explicit override can name a file that is not there, and
        // pointing at nothing is a typo worth reporting.
        return if super::super::resolve_setting(proxy, HOOKS_CONFIG_KEY).is_some() {
            Err(format!(
                "chat: {HOOKS_CONFIG_KEY} points at {}, which is not a file",
                path.display()
            ))
        } else {
            Ok(LoadedHooks::default())
        };
    }

    let signature = signature(&path)?;
    if let Some(cached) = CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.clone())
        .filter(|(cached, _)| *cached == signature)
        .map(|(_, hooks)| hooks)
    {
        return Ok(cached);
    }

    let hooks = read(&path)?;

    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((signature, hooks.clone()));
    }

    Ok(hooks)
}

fn signature(path: &Path) -> Result<FileSignature, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("chat: cannot read {}: {err}", path.display()))?;
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);

    Ok(FileSignature {
        path: path.to_path_buf(),
        modified_ms,
        len: metadata.len(),
    })
}

pub(crate) fn read(path: &Path) -> Result<LoadedHooks, String> {
    check_permissions(path)?;

    let contents = std::fs::read_to_string(path)
        .map_err(|err| format!("chat: cannot read {}: {err}", path.display()))?;

    parse(&contents).map_err(|err| {
        format!(
            "chat: {} is not usable: {err}. Fix it, or set {HOOKS_ENABLED_KEY}=off. `doctor hooks` shows what was read.",
            path.display()
        )
    })
}

/// A world-writable list of commands to run is a way in.
///
/// **World-writable only.** Group-writable was refused too at first, which
/// looked stricter but was a trap: a `umask` of 002 - the default for
/// per-user-group distributions - creates every new file as 664, so writing an
/// `ai-hooks.json` the ordinary way made the whole `!` chat refuse to start.
/// On those systems the group is the user's own, so 664 grants nobody anything.
/// Being stricter than `config.lisp` (which has no check at all) by an amount
/// that breaks the feature on common systems is the wrong trade.
///
/// The containing directory counts as well: a world-writable directory lets
/// anyone replace the file whatever its own mode says.
///
/// Both supported platforms are Unix, so this needs no `cfg` pair.
fn check_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let refuse = |what: &Path, mode: u32| {
        format!(
            "chat: {} is world-writable (mode {:o}); run `chmod o-w {}` before hooks will load",
            what.display(),
            mode & 0o777,
            what.display()
        )
    };

    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("chat: cannot read {}: {err}", path.display()))?;
    let mode = metadata.permissions().mode();
    if mode & 0o002 != 0 {
        return Err(refuse(path, mode));
    }

    if let Some(parent) = path.parent()
        && let Ok(parent_metadata) = std::fs::metadata(parent)
    {
        let parent_mode = parent_metadata.permissions().mode();
        // A sticky directory (`/tmp`) only lets the owner replace their own
        // file, so it is not the hole this is looking for.
        if parent_mode & 0o002 != 0 && parent_mode & 0o1000 == 0 {
            return Err(refuse(parent, parent_mode));
        }
    }

    Ok(())
}

pub(crate) fn parse(contents: &str) -> Result<LoadedHooks, String> {
    if contents.trim().is_empty() {
        return Ok(LoadedHooks::default());
    }

    let file: HooksFile = serde_json::from_str(contents).map_err(|err| err.to_string())?;

    if file.version != 1 {
        return Err(format!(
            "unsupported version {}; this shell understands version 1",
            file.version
        ));
    }
    if !file.enabled {
        return Ok(LoadedHooks::default());
    }

    let mut seen = BTreeSet::new();
    for hook in &file.hooks {
        if hook.id.is_empty()
            || !hook
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return Err(format!(
                "hook id `{}` must be lowercase letters, digits, `-` or `_`",
                hook.id
            ));
        }
        if !seen.insert(hook.id.clone()) {
            return Err(format!(
                "hook id `{}` is used twice; ids name hooks in messages and approvals",
                hook.id
            ));
        }
        if hook.events.is_empty() {
            return Err(format!("hook `{}` lists no events", hook.id));
        }
        if hook.command.is_empty() || hook.command[0].trim().is_empty() {
            return Err(format!("hook `{}` has an empty command", hook.id));
        }
        if let Some(matcher) = &hook.matcher
            && let Some(bad) = matcher.tools.iter().find(|pattern| {
                // `trim_end_matches` strips *every* trailing star, so `mcp**`
                // passed validation and then matched nothing at all - the
                // silent no-op this module exists to prevent.
                pattern.strip_suffix('*').unwrap_or(pattern).contains('*')
            })
        {
            return Err(format!(
                "hook `{}`: `{bad}` may only use `*` as the last character",
                hook.id
            ));
        }
    }

    for event in [
        HookEvent::SessionStart,
        HookEvent::UserPromptSubmit,
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::ResponseComplete,
    ] {
        let count = file
            .hooks
            .iter()
            .filter(|hook| hook.enabled && hook.events.contains(&event))
            .count();
        if count > MAX_HOOKS_PER_EVENT {
            return Err(format!(
                "{count} hooks on `{}`; at most {MAX_HOOKS_PER_EVENT} may run for one event",
                event.as_str()
            ));
        }
    }

    let mut hooks = file.hooks;
    for hook in &mut hooks {
        hook.command[0] = normalize_program(&hook.command[0])
            .map_err(|err| format!("hook `{}`: {err}", hook.id))?;
    }

    Ok(LoadedHooks { hooks })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(command: &str) -> String {
        format!(
            r#"{{"version":1,"hooks":[{{"id":"audit","events":["pre-tool-use"],"command":{command}}}]}}"#
        )
    }

    #[test]
    fn parses_a_minimal_hook_definition() {
        let hooks = parse(&one(r#"["/opt/dsh-hooks/hook.sh"]"#)).unwrap();
        let matched = hooks.matching(HookEvent::PreToolUse, Some("execute"));
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "audit");
        assert_eq!(matched[0].timeout_ms(), 5_000);
    }

    /// A field name that does nothing is a hook the author believes is running.
    #[test]
    fn unknown_field_is_a_parse_error() {
        let err = parse(
            r#"{"version":1,"hooks":[{"id":"a","event":["pre-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .expect_err("a typo must not load quietly");
        assert!(err.contains("event"), "{err}");
    }

    #[test]
    fn string_command_is_rejected_with_an_array_hint() {
        let err = parse(&one(r#""python3 hook.py""#)).expect_err("a string is not an argv");
        assert!(err.contains("array of strings"), "{err}");
        assert!(err.contains("not through a shell"), "{err}");
    }

    #[test]
    fn empty_command_array_is_rejected() {
        let err = parse(&one("[]")).expect_err("nothing to run");
        assert!(err.contains("empty command"), "{err}");
    }

    #[test]
    fn duplicate_hook_id_is_rejected() {
        let err = parse(
            r#"{"version":1,"hooks":[
                {"id":"a","events":["pre-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]},
                {"id":"a","events":["post-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .expect_err("ids name hooks in approvals");
        assert!(err.contains("used twice"), "{err}");
    }

    #[test]
    fn unknown_event_name_is_a_parse_error() {
        let err = parse(
            r#"{"version":1,"hooks":[{"id":"a","events":["PreToolUse"],"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .expect_err("event names are kebab-case");
        assert!(
            err.contains("PreToolUse") || err.contains("unknown variant"),
            "{err}"
        );
    }

    #[test]
    fn version_other_than_one_is_rejected() {
        let err = parse(r#"{"version":2,"hooks":[]}"#).expect_err("unknown schema");
        assert!(err.contains("version 1"), "{err}");
    }

    #[test]
    fn timeout_is_clamped_to_the_supported_range() {
        let hooks = parse(
            r#"{"version":1,"hooks":[
                {"id":"slow","events":["post-tool-use"],"command":["/opt/dsh-hooks/hook.sh"],"timeout_ms":9999999},
                {"id":"fast","events":["post-tool-use"],"command":["/opt/dsh-hooks/hook.sh"],"timeout_ms":1}]}"#,
        )
        .unwrap();
        let all = hooks.all();
        assert_eq!(all[0].timeout_ms(), MAX_TIMEOUT_MS);
        assert_eq!(all[1].timeout_ms(), MIN_TIMEOUT_MS);
    }

    #[test]
    fn more_than_eight_hooks_for_one_event_is_rejected() {
        let entries = (0..9)
            .map(|i| {
                format!(r#"{{"id":"h{i}","events":["pre-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]}}"#)
            })
            .collect::<Vec<_>>()
            .join(",");
        let err = parse(&format!(r#"{{"version":1,"hooks":[{entries}]}}"#))
            .expect_err("one tool call must not wait for nine processes");
        assert!(err.contains("at most 8"), "{err}");
    }

    #[test]
    fn a_disabled_file_loads_nothing() {
        let hooks = parse(
            r#"{"version":1,"enabled":false,"hooks":[{"id":"a","events":["pre-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .unwrap();
        assert!(hooks.is_empty());
    }

    #[test]
    fn a_disabled_hook_never_matches() {
        let hooks = parse(
            r#"{"version":1,"hooks":[{"id":"a","events":["pre-tool-use"],"command":["/opt/dsh-hooks/hook.sh"],"enabled":false}]}"#,
        )
        .unwrap();
        assert!(
            hooks
                .matching(HookEvent::PreToolUse, Some("execute"))
                .is_empty()
        );
    }

    #[test]
    fn tool_match_glob_matches_an_mcp_prefix() {
        let hooks = parse(
            r#"{"version":1,"hooks":[{"id":"a","events":["pre-tool-use"],"match":{"tools":["mcp__*","edit"]},"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .unwrap();

        assert_eq!(
            hooks
                .matching(HookEvent::PreToolUse, Some("mcp__x__y"))
                .len(),
            1
        );
        assert_eq!(hooks.matching(HookEvent::PreToolUse, Some("edit")).len(), 1);
        assert!(
            hooks
                .matching(HookEvent::PreToolUse, Some("execute"))
                .is_empty()
        );
        // No tool at all cannot satisfy a tool matcher.
        assert!(hooks.matching(HookEvent::PreToolUse, None).is_empty());
    }

    /// Only a trailing `*` is supported. `mcp**` used to pass validation and
    /// then match nothing, which is the silent no-op this module exists to
    /// prevent.
    #[test]
    fn a_star_anywhere_but_the_end_is_rejected_rather_than_taken_literally() {
        fn config(pattern: &str) -> String {
            format!(
                r#"{{"version":1,"hooks":[{{"id":"a","events":["pre-tool-use"],"match":{{"tools":["{pattern}"]}},"command":["/opt/dsh-hooks/hook.sh"]}}]}}"#
            )
        }

        for pattern in ["mc*p", "mcp**", "*mcp"] {
            let err = parse(&config(pattern))
                .err()
                .unwrap_or_else(|| panic!("`{pattern}` must be refused"));
            assert!(err.contains("last character"), "{pattern}: {err}");
        }

        // The one supported shape still loads and still matches.
        let hooks = parse(&config("mcp__*")).expect("a trailing star is the supported form");
        assert_eq!(
            hooks
                .matching(HookEvent::PreToolUse, Some("mcp__server__tool"))
                .len(),
            1
        );
    }

    #[test]
    fn the_two_events_with_nowhere_to_put_context_say_so() {
        assert!(HookEvent::UserPromptSubmit.uses_context());
        assert!(HookEvent::PreToolUse.uses_context());
        assert!(HookEvent::PostToolUse.uses_context());
        assert!(!HookEvent::SessionStart.uses_context());
        assert!(!HookEvent::ResponseComplete.uses_context());
    }

    #[test]
    fn only_prompt_and_pre_tool_events_can_stop_anything() {
        assert!(HookEvent::UserPromptSubmit.is_gate());
        assert!(HookEvent::PreToolUse.is_gate());
        assert!(!HookEvent::SessionStart.is_gate());
        assert!(!HookEvent::PostToolUse.is_gate());
        assert!(!HookEvent::ResponseComplete.is_gate());
    }

    /// 664 is what `umask 002` produces, and on those systems the group is the
    /// user's own. Refusing it meant creating the file the ordinary way stopped
    /// `!` from working at all.
    #[test]
    fn a_group_writable_config_loads_but_a_world_writable_one_does_not() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.path().join("ai-hooks.json");
        std::fs::write(&path, r#"{"version":1,"hooks":[]}"#).unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
        assert!(read(&path).is_ok(), "umask 002 must not brick the chat");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let err = read(&path).expect_err("a world-writable command list is a way in");
        assert!(err.contains("world-writable"), "{err}");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(&path).is_ok());
    }

    /// The file's own mode is no protection when anyone can replace it.
    #[test]
    fn a_world_writable_directory_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("conf");
        std::fs::create_dir(&nested).unwrap();
        let path = nested.join("ai-hooks.json");
        std::fs::write(&path, r#"{"version":1,"hooks":[]}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o777)).unwrap();

        let err = read(&path).expect_err("anyone could swap the file");
        assert!(err.contains("world-writable"), "{err}");

        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(read(&path).is_ok());
    }

    /// A relative program is resolved after the runner chdirs, so `./hook.sh`
    /// meant "whatever sits in the repository the user cd'd into".
    #[test]
    fn a_relative_hook_command_is_refused_at_load_time() {
        for program in ["./hook.sh", "../hook.sh", "hooks/audit.sh"] {
            let err = parse(&one(&format!(r#"["{program}"]"#)))
                .err()
                .unwrap_or_else(|| panic!("`{program}` must be refused"));
            assert!(err.contains("relative path"), "{program}: {err}");
        }
    }

    #[test]
    fn a_bare_command_name_is_pinned_to_its_path_entry() {
        let hooks = parse(&one(r#"["true"]"#)).expect("a PATH name is allowed");
        let program = &hooks.all()[0].command[0];
        // Either resolved to an absolute path, or left alone when not on PATH -
        // never left as something a later chdir could reinterpret.
        assert!(
            Path::new(program).is_absolute() || program == "true",
            "{program}"
        );
    }

    #[test]
    fn an_empty_file_is_not_an_error() {
        assert!(parse("   \n").unwrap().is_empty());
    }

    /// The file is read once per turn at most, so an edit has to invalidate the
    /// cache rather than wait for a new shell.
    #[test]
    fn cache_reloads_when_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai-hooks.json");

        std::fs::write(&path, r#"{"version":1,"hooks":[]}"#).unwrap();
        clear_cache();
        assert!(read(&path).unwrap().is_empty());

        std::fs::write(
            &path,
            r#"{"version":1,"hooks":[{"id":"a","events":["post-tool-use"],"command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .unwrap();
        let reloaded = read(&path).unwrap();
        assert_eq!(reloaded.all().len(), 1);
    }
}
