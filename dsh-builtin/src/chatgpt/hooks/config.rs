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
/// Both supported platforms are Unix, so this needs no `cfg` pair. The rule is
/// stricter than the one on `config.lisp` because every line here names a
/// program that the shell will execute on the user's behalf.
fn check_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("chat: cannot read {}: {err}", path.display()))?;
    let mode = metadata.permissions().mode();
    if mode & 0o022 != 0 {
        return Err(format!(
            "chat: {} is writable by other users (mode {:o}); run `chmod go-w {}` before hooks will load",
            path.display(),
            mode & 0o777,
            path.display()
        ));
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

    Ok(LoadedHooks { hooks: file.hooks })
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
        let hooks = parse(&one(r#"["./hook.sh"]"#)).unwrap();
        let matched = hooks.matching(HookEvent::PreToolUse, Some("execute"));
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "audit");
        assert_eq!(matched[0].timeout_ms(), 5_000);
    }

    /// A field name that does nothing is a hook the author believes is running.
    #[test]
    fn unknown_field_is_a_parse_error() {
        let err = parse(
            r#"{"version":1,"hooks":[{"id":"a","event":["pre-tool-use"],"command":["./hook.sh"]}]}"#,
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
                {"id":"a","events":["pre-tool-use"],"command":["./hook.sh"]},
                {"id":"a","events":["post-tool-use"],"command":["./hook.sh"]}]}"#,
        )
        .expect_err("ids name hooks in approvals");
        assert!(err.contains("used twice"), "{err}");
    }

    #[test]
    fn unknown_event_name_is_a_parse_error() {
        let err = parse(
            r#"{"version":1,"hooks":[{"id":"a","events":["PreToolUse"],"command":["./hook.sh"]}]}"#,
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
                {"id":"slow","events":["post-tool-use"],"command":["./hook.sh"],"timeout_ms":9999999},
                {"id":"fast","events":["post-tool-use"],"command":["./hook.sh"],"timeout_ms":1}]}"#,
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
                format!(r#"{{"id":"h{i}","events":["pre-tool-use"],"command":["./hook.sh"]}}"#)
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
            r#"{"version":1,"enabled":false,"hooks":[{"id":"a","events":["pre-tool-use"],"command":["./hook.sh"]}]}"#,
        )
        .unwrap();
        assert!(hooks.is_empty());
    }

    #[test]
    fn a_disabled_hook_never_matches() {
        let hooks = parse(
            r#"{"version":1,"hooks":[{"id":"a","events":["pre-tool-use"],"command":["./hook.sh"],"enabled":false}]}"#,
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
            r#"{"version":1,"hooks":[{"id":"a","events":["pre-tool-use"],"match":{"tools":["mcp__*","edit"]},"command":["./hook.sh"]}]}"#,
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
                r#"{{"version":1,"hooks":[{{"id":"a","events":["pre-tool-use"],"match":{{"tools":["{pattern}"]}},"command":["./hook.sh"]}}]}}"#
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

    #[test]
    fn group_writable_config_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai-hooks.json");
        std::fs::write(&path, r#"{"version":1,"hooks":[]}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();

        let err = read(&path).expect_err("a world-writable command list is a way in");
        assert!(err.contains("writable by other users"), "{err}");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(&path).is_ok());
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
            r#"{"version":1,"hooks":[{"id":"a","events":["post-tool-use"],"command":["./hook.sh"]}]}"#,
        )
        .unwrap();
        let reloaded = read(&path).unwrap();
        assert_eq!(reloaded.all().len(), 1);
    }
}
