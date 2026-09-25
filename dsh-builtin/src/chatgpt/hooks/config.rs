//! Reading `ai-hooks.json`.
//!
//! Strict on purpose. A hook that silently does not fire is worse than no hook:
//! the person who wrote it believes a check is running. So an unknown field, an
//! unknown event name, a duplicate id and a `command` written as a string are
//! all load errors, and a load error refuses the chat rather than continuing
//! without the hooks.

use serde::{Deserialize, Deserializer, de};
use serde_json::Value;
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::UNIX_EPOCH;

use crate::ShellProxy;

mod load;
use load::deserialize_command;
pub(crate) use load::{
    LoadedHooks, config_path, enabled, load, nested_in_a_hook, read, turn_budget_ms,
};
#[cfg(test)]
pub(crate) use load::{clear_cache, parse};

/// The configuration file, looked up like every other `dogesh` config file.
pub(crate) const HOOKS_CONFIG_FILE: &str = "ai-hooks.json";
/// Points at a different file. Resolved shell-variable first, like every other
/// AI setting.
pub(crate) const HOOKS_CONFIG_KEY: &str = "DOGESH_AI_HOOKS_CONFIG";
/// `0` / `false` / `off` / `no` stops the file from being read at all.
pub(crate) const HOOKS_ENABLED_KEY: &str = "AI_CHAT_HOOKS";
/// Set on every hook process. A hook that starts another `dogesh` must not have
/// that shell run hooks of its own.
pub(crate) const HOOK_DEPTH_ENV: &str = "DOGESH_HOOK_DEPTH";
/// A ceiling on the wall time one turn may spend waiting for hooks.
///
/// Opt-in, and unlimited when unset, the same shape as
/// `AI_CHAT_TURN_TOKEN_BUDGET`. A default would mean hooks quietly stopping at
/// some point in a long turn, and "a check that silently stopped running" is
/// the failure this module refuses everywhere else.
pub(crate) const HOOK_TURN_BUDGET_KEY: &str = "AI_CHAT_HOOK_TURN_BUDGET_MS";

const DEFAULT_TIMEOUT_MS: u64 = 5_000;
pub(crate) const MIN_TIMEOUT_MS: u64 = 100;
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
    PreCompact,
    ResponseComplete,
}

impl HookEvent {
    /// Every event, so that a check written "for all events" stays that way.
    ///
    /// `parse` used to spell the list out where it enforces
    /// `MAX_HOOKS_PER_EVENT`, and a list that has to be edited alongside the
    /// enum is a list that will not be.
    pub(crate) const ALL: [HookEvent; 6] = [
        HookEvent::SessionStart,
        HookEvent::UserPromptSubmit,
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::PreCompact,
        HookEvent::ResponseComplete,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session-start",
            HookEvent::UserPromptSubmit => "user-prompt-submit",
            HookEvent::PreToolUse => "pre-tool-use",
            HookEvent::PostToolUse => "post-tool-use",
            HookEvent::PreCompact => "pre-compact",
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

    /// Does this event carry a tool call for a matcher to look at?
    ///
    /// The events that do not carry one cannot satisfy an argument matcher, so
    /// `doctor hooks` says so rather than leaving the author to notice their
    /// hook never fires on half its events.
    pub(crate) fn carries_a_tool(self) -> bool {
        matches!(self, HookEvent::PreToolUse | HookEvent::PostToolUse)
    }

    /// Does this event have somewhere to put `additional_context`?
    ///
    /// The test is not "will anyone read it later" but "is there a place to put
    /// it that changes nothing about a control decision".
    ///
    /// `session-start` would have to change the system prompt, which decides
    /// whether the previous conversation is carried forward - a hook whose
    /// output varied would then silently end the conversation every turn.
    /// `response-complete` happens after the last thing the model reads.
    /// `pre-compact` has only the buffer, and text added there is both about to
    /// be compacted and enough to keep `should_summarize` true - which can buy
    /// the user another paid summary round.
    pub(crate) fn uses_context(self) -> bool {
        matches!(
            self,
            HookEvent::UserPromptSubmit | HookEvent::PreToolUse | HookEvent::PostToolUse
        )
    }
}

/// Which calls a hook actually wants.
///
/// `dogesh` funnels every command through the one `execute` tool, so
/// `{"tools":["execute"]}` means "every command" and a hook written to watch
/// `rm` paid its timeout on every `ls`. The extra kinds below let a hook say
/// what it is really watching.
///
/// **Kinds are ANDed, entries within a kind are ORed.** `tools` was already an
/// OR, so that is the reading a config author already has.
///
/// Still not a regular expression. `programs` reuses the word-prefix form of
/// `AI_CHAT_EXECUTE_ALLOWLIST` and `paths` uses a glob, both of which a person
/// can read back correctly at a glance; a regex in a security-adjacent config
/// is a thing people get subtly wrong and then trust.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HookMatch {
    /// Tool names, with `*` allowed only as the last character (`mcp__*`).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Programs the command runs, in `AI_CHAT_EXECUTE_ALLOWLIST` form.
    /// `execute` only; wrappers are looked through and every stage is judged.
    #[serde(default)]
    pub programs: Vec<String>,
    /// Globs against the paths the call names.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Exact values for top-level argument fields.
    #[serde(default)]
    pub arguments: BTreeMap<String, Value>,
}

impl HookMatch {
    fn is_empty(&self) -> bool {
        self.tools.is_empty()
            && self.programs.is_empty()
            && self.paths.is_empty()
            && self.arguments.is_empty()
    }

    fn matches_tools(&self, input: &MatchInput<'_>) -> bool {
        if self.tools.is_empty() {
            return true;
        }
        let Some(tool) = input.tool() else {
            // A tool matcher on an event that carries no tool cannot be
            // satisfied; firing anyway would be a surprise in the permissive
            // direction.
            return false;
        };
        self.tools.iter().any(|pattern| match_tool(pattern, tool))
    }

    fn matches_programs(&self, input: &MatchInput<'_>) -> bool {
        if self.programs.is_empty() {
            return true;
        }
        match input.command_line() {
            // Unreadable arguments fire. See `MatchInput`.
            Field::Unreadable => true,
            Field::Missing => false,
            Field::Present(command) => {
                super::super::tool::execute::command_names_any(command, &self.programs)
            }
        }
    }

    fn matches_paths(&self, input: &MatchInput<'_>) -> bool {
        if self.paths.is_empty() {
            return true;
        }
        let candidates = match input.path_candidates() {
            Field::Unreadable => return true,
            Field::Missing => return false,
            Field::Present(candidates) => candidates,
        };
        if candidates.is_empty() {
            return false;
        }
        self.paths.iter().any(|pattern| {
            // Compiled here, not at load time: `LoadedHooks` is cached by file
            // signature and `glob::Pattern` is neither `Deserialize` nor worth
            // a parallel structure. `parse` already proved every pattern
            // compiles, so this cannot fail in a way that hides a hook.
            glob::Pattern::new(pattern).is_ok_and(|glob| {
                candidates
                    .iter()
                    .any(|candidate| glob.matches(candidate.as_str()))
            })
        })
    }

    fn matches_arguments(&self, input: &MatchInput<'_>) -> bool {
        if self.arguments.is_empty() {
            return true;
        }
        let arguments = match input.parsed() {
            Field::Unreadable => return true,
            Field::Missing => return false,
            Field::Present(value) => value,
        };
        self.arguments
            .iter()
            .all(|(field, expected)| arguments.get(field) == Some(expected))
    }
}

/// The three answers a matcher can get about a value it wants to look at.
///
/// `Missing` and `Unreadable` are kept apart because they point opposite ways.
/// `Missing` is structural - a `session-start` event has no tool arguments and
/// never will - so a matcher about arguments simply is not satisfied, the same
/// reading `tools` already has. `Unreadable` means the data is there and cannot
/// be read, and then the matcher fires: a mismatched quote must not be a way to
/// skip a check the user asked for.
enum Field<T> {
    Missing,
    Unreadable,
    Present(T),
}

/// Argument fields that name a path, per tool.
///
/// A table rather than a guess, for the tools whose schemas this crate owns.
/// Everything else - MCP included - falls back to
/// `MatchInput::inferred_path_fields`, which over-includes on purpose: a hook
/// firing when it need not have costs a hook run, and a hook not firing when it
/// should have costs the check.
const PATH_ARGUMENT_FIELDS: &[(&str, &[&str])] = &[
    ("edit", &["path"]),
    ("read_file", &["path"]),
    ("str_replace", &["path"]),
    ("ls", &["path"]),
    ("search", &["path"]),
    // `file` is relative to the *skill directory*, not to the chat's working
    // directory, so joining it with the latter tests a path the call never
    // touches. Resolving it properly would mean re-deriving the skill root
    // here, which `ai-architecture.md` §7 keeps to `skill_roots` alone. A hook
    // that wants to watch skill writes matches `tools: ["skill_manage"]`.
    ("skill_manage", &[]),
];

/// What a `match` clause is judged against.
///
/// Plain copyable data: the tool's name and the arguments the model sent.
/// Those arguments are the **unmasked** ones. Matching happens inside this
/// process and the value never reaches a hook - what the hook reads still goes
/// through `tool_detail`'s masking - because masking is lossy in the
/// *permissive* direction for a matcher. `safety_policy::SECRET_OPTION` rewrites
/// `-p <value>` unconditionally, so `mkdir -p /etc/myapp` masks to
/// `mkdir -p ***` and a hook watching `/etc/**` would never fire. A gate that
/// silently stops firing is the failure this file's doctrine exists to prevent.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HookSubject<'a> {
    pub tool: Option<&'a str>,
    pub arguments: Option<&'a str>,
}

impl<'a> HookSubject<'a> {
    /// An event that carries no tool, so no tool matcher can be true of it.
    pub(crate) fn none() -> Self {
        Self::default()
    }

    pub(crate) fn tool(name: &'a str, arguments: &'a str) -> Self {
        Self {
            tool: Some(name),
            arguments: Some(arguments),
        }
    }
}

/// A subject plus whatever had to be derived from it to answer a `match`.
///
/// Built **once per `fire`**, never once per hook: the whole point of `match` is
/// to spend less, and re-parsing a command for each of eight hooks would spend
/// more than the filter saves. Each derived form is a `OnceCell`, so a config
/// that only uses `tools` never touches the arguments at all.
pub(crate) struct MatchInput<'a> {
    subject: HookSubject<'a>,
    /// The directory the call will run in, for turning a relative path
    /// argument into something a `/etc/**` glob can match.
    cwd: &'a Path,
    parsed: OnceCell<Option<Value>>,
    paths: OnceCell<Option<Vec<String>>>,
}

impl<'a> MatchInput<'a> {
    pub(crate) fn new(subject: HookSubject<'a>, cwd: &'a Path) -> Self {
        Self {
            subject,
            cwd,
            parsed: OnceCell::new(),
            paths: OnceCell::new(),
        }
    }

    fn tool(&self) -> Option<&'a str> {
        self.subject.tool
    }

    fn parsed(&self) -> Field<&Value> {
        // Empty is *absent*, not unreadable. `execute_tool_call` builds the
        // string with `unwrap_or_default`, so a provider that omits `arguments`
        // for a no-argument tool arrives here as `""`; calling that unreadable
        // made every argument matcher fire on those calls.
        let Some(raw) = self.subject.arguments.filter(|raw| !raw.trim().is_empty()) else {
            return Field::Missing;
        };
        match self
            .parsed
            .get_or_init(|| serde_json::from_str::<Value>(raw).ok())
        {
            Some(value) => Field::Present(value),
            None => Field::Unreadable,
        }
    }

    /// The `command` string of an `execute` call.
    fn command_line(&self) -> Field<&str> {
        match self.parsed() {
            Field::Missing => Field::Missing,
            Field::Unreadable => Field::Unreadable,
            Field::Present(value) => match value.get("command").and_then(Value::as_str) {
                Some(command) => Field::Present(command),
                None => Field::Missing,
            },
        }
    }

    /// Every path this call names, both as written and lexically absolute.
    ///
    /// Lexical only - never `canonicalize`. Resolving would `stat` paths the
    /// model chose, which is a side effect and a symlink race brought into
    /// deciding *which hook to run*. A symlink can therefore dodge a `paths`
    /// matcher; that is a missed check, never a false permit, because a hook
    /// cannot grant anything. The real path decisions stay with `SafetyGuard`
    /// and `reject_gitignored_read_path`.
    fn path_candidates(&self) -> Field<&[String]> {
        let arguments = match self.parsed() {
            Field::Missing => return Field::Missing,
            Field::Unreadable => return Field::Unreadable,
            Field::Present(value) => value,
        };
        let built = self.paths.get_or_init(|| {
            let mut raw: Vec<String> = Vec::new();
            // Where a relative token lands. `execute` takes a `cwd` argument and
            // the command runs there, so resolving against the shell's
            // directory instead is the same hole `touches_skill_file` closed
            // for skill scripts: `{"command":"cat sub/x","cwd":"/etc"}`.
            let mut base = self.cwd.to_path_buf();

            if self.tool() == Some("execute") {
                if let Some(cwd) = arguments.get("cwd").and_then(Value::as_str) {
                    // `join` with an absolute path replaces, so this covers
                    // both an absolute and a relative `cwd`.
                    base = base.join(cwd);
                    raw.push(cwd.to_string());
                }
                // `None` here means the command line could not be tokenised,
                // which has to reach the matcher as `Unreadable` rather than
                // as "names no paths".
                let command = arguments.get("command").and_then(Value::as_str);
                match command.map(super::super::tool::execute::command_tokens) {
                    Some(None) => return None,
                    Some(Some(tokens)) => raw.extend(
                        // Options are not paths; a real path token starting
                        // with `-` would have to be written `./-foo`. Every
                        // other token is kept even though a bare word is more
                        // often a program name than a file: `rm .env` and
                        // `rm ./.env` must both reach a hook watching
                        // `**/*.env`, and there is no way to tell a bare
                        // filename from a bare program name. So this
                        // over-includes, like everything else here, because a
                        // hook that fires needlessly costs one hook run while
                        // one that stays quiet costs the check.
                        tokens.into_iter().filter(|token| !token.starts_with('-')),
                    ),
                    None => {}
                }
            }

            let fields = PATH_ARGUMENT_FIELDS
                .iter()
                .find(|(tool, _)| Some(*tool) == self.tool())
                .map(|(_, fields)| *fields);
            match fields {
                // An empty list means "this crate knows the tool and it names
                // no path", which must not fall through to inference.
                Some(fields) => raw.extend(
                    fields
                        .iter()
                        .filter_map(|field| arguments.get(*field).and_then(Value::as_str))
                        .map(str::to_string),
                ),
                None if self.tool() != Some("execute") => {
                    raw.extend(Self::inferred_path_fields(arguments))
                }
                None => {}
            }

            Some(Self::expand_path_candidates(raw, &base))
        });
        match built {
            Some(candidates) => Field::Present(candidates.as_slice()),
            None => Field::Unreadable,
        }
    }

    /// Path-shaped strings anywhere in the arguments, for tools whose schema
    /// this crate does not own. Keeping a table of every MCP server's arguments
    /// is not a thing anyone can maintain, so shape stands in for a
    /// declaration.
    ///
    /// Every string leaf, not just the top-level ones: an MCP tool is as likely
    /// to take `{"paths": ["/etc/shadow"]}` or `{"target": {"path": "..."}}` as
    /// a flat field, and a matcher that quietly misses those is the no-op this
    /// module exists to prevent. Depth and count are capped so a pathological
    /// payload cannot turn hook *selection* into an unbounded walk.
    fn inferred_path_fields(arguments: &Value) -> Vec<String> {
        const MAX_DEPTH: usize = 6;
        const MAX_CANDIDATES: usize = 64;

        fn walk(value: &Value, depth: usize, out: &mut Vec<String>) {
            if depth > MAX_DEPTH || out.len() >= MAX_CANDIDATES {
                return;
            }
            match value {
                Value::String(text) if text.contains('/') || text.starts_with('~') => {
                    out.push(text.clone())
                }
                Value::Array(items) => {
                    for item in items {
                        walk(item, depth + 1, out);
                    }
                }
                Value::Object(fields) => {
                    for field in fields.values() {
                        walk(field, depth + 1, out);
                    }
                }
                _ => {}
            }
        }

        let mut out = Vec::new();
        walk(arguments, 0, &mut out);
        out
    }

    fn expand_path_candidates(raw: Vec<String>, cwd: &Path) -> Vec<String> {
        let mut out = Vec::with_capacity(raw.len() * 2);
        for token in raw {
            if token.is_empty() {
                continue;
            }
            let expanded = shellexpand::full(&token)
                .map(|value| value.into_owned())
                .unwrap_or_else(|_| token.clone());
            let path = Path::new(&expanded);
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            };
            let normalized = super::super::tool::normalize_path(&absolute);
            out.push(token);
            if let Some(text) = normalized.to_str() {
                out.push(text.to_string());
            }
        }
        out.sort();
        out.dedup();
        out
    }
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

    /// Does every kind of matcher this hook declares hold for `input`?
    ///
    /// Kinds are ANDed, entries within a kind are ORed.
    fn matches(&self, input: &MatchInput<'_>) -> bool {
        let Some(matcher) = &self.matcher else {
            return true;
        };
        matcher.matches_tools(input)
            && matcher.matches_programs(input)
            && matcher.matches_paths(input)
            && matcher.matches_arguments(input)
    }

    /// Does this hook carry a `match` that narrows nothing?
    ///
    /// Not a load error - `{"tools": []}` has always meant "every call" - but
    /// worth saying, because it usually means the author expected it to filter.
    pub(crate) fn matcher_narrows_nothing(&self) -> bool {
        self.matcher.as_ref().is_some_and(HookMatch::is_empty)
    }

    /// Which of this hook's matcher kinds need arguments to be satisfiable.
    ///
    /// Used by `doctor hooks` to say so, not to refuse the configuration: a
    /// hook listing `session-start` alongside `pre-tool-use` is a reasonable
    /// thing to write, and refusing it would break setups that work today.
    pub(crate) fn argument_matcher_kinds(&self) -> Vec<&'static str> {
        let Some(matcher) = &self.matcher else {
            return Vec::new();
        };
        let mut kinds = Vec::new();
        if !matcher.tools.is_empty() {
            kinds.push("tools");
        }
        if !matcher.programs.is_empty() {
            kinds.push("programs");
        }
        if !matcher.paths.is_empty() {
            kinds.push("paths");
        }
        if !matcher.arguments.is_empty() {
            kinds.push("arguments");
        }
        kinds
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

/// Refuse a matcher that can never be true, or that is true in a way its author
/// did not write.
///
/// Every check here exists because the alternative is a hook that loads, reports
/// clean in `doctor hooks`, and never fires. That is the one failure this module
/// is built to prevent, so it is worth refusing the whole chat over.
fn validate_matcher(id: &str, matcher: &HookMatch) -> Result<(), String> {
    // An empty `match` is *not* an error. `{"tools": []}` has always meant
    // "every call", and a configuration that works today must not start
    // refusing the whole chat. `doctor hooks` points it out instead - the same
    // treatment the unsatisfiable-per-event case gets.

    if !matcher.programs.is_empty() {
        // `programs` reads the `command` argument, which only `execute` has.
        // Without an `execute`-shaped `tools` entry the matcher is dead
        // configuration that looks like a filter.
        if !matcher
            .tools
            .iter()
            .any(|pattern| match_tool(pattern, "execute"))
        {
            return Err(format!(
                "hook `{id}`: `programs` needs `\"tools\": [\"execute\"]`; \
                 only the execute tool carries a command line"
            ));
        }
        for entry in &matcher.programs {
            match shell_words::split(entry) {
                Ok(tokens) if !tokens.is_empty() => {}
                _ => {
                    return Err(format!(
                        "hook `{id}`: `programs` entry `{entry}` is not a command; \
                         write it the way `AI_CHAT_EXECUTE_ALLOWLIST` does, e.g. \"git push\""
                    ));
                }
            }
        }
    }

    for pattern in &matcher.paths {
        glob::Pattern::new(pattern).map_err(|err| {
            format!("hook `{id}`: `paths` entry `{pattern}` is not a glob: {err}")
        })?;
    }

    for (field, value) in &matcher.arguments {
        if !matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
            return Err(format!(
                "hook `{id}`: `arguments.{field}` must be a string, number or boolean; \
                 use `paths` for a path and `programs` for a command"
            ));
        }
    }

    Ok(())
}

/// Pin `command[0]` to something a later `chdir` cannot reinterpret.
///
/// The runner sets `current_dir` to the chat's working directory, and on Unix a
/// relative program is resolved *after* that. `["/opt/dsh-hooks/hook.sh"]` therefore ran
/// whatever `./hook.sh` happened to sit in the repository the user had cd'd
/// into - which is exactly the "cloning a repository should not be enough to
/// run its commands" case that keeps `.dogesh/hooks.json` unread.
///
/// - absolute path: kept as is
/// - bare name (no `/`): resolved against the logical runtime snapshot's
///   command search paths **now**, so the lookup cannot be steered by the
///   directory the hook later runs in. Left alone when it is not on the
///   logical PATH; the spawn fails with a clear error and `doctor hooks`
///   already reports it, which beats refusing every chat over a typo.
/// - anything else (`./x`, `../x`, `a/b`): refused
pub(crate) fn pin_program(
    snapshot: &dsh_types::process_runtime::CommandRuntimeSnapshot,
    program: &str,
) -> Result<String, String> {
    let path = Path::new(program);
    if path.is_absolute() {
        return Ok(program.to_string());
    }
    if program.contains('/') {
        return Err(format!(
            "`{program}` is a relative path; write an absolute path, or a bare name to look up on PATH. A relative one resolves against whatever directory the chat is in when the hook runs"
        ));
    }

    Ok(snapshot
        .resolve_bare_program(program)
        .map(|candidate| candidate.display().to_string())
        .unwrap_or_else(|| program.to_string()))
}

/// The load-time half of [`pin_program`] that needs no runtime: relative
/// pathnames are refused while parsing so a bad config fails fast even
/// before a snapshot exists.
pub(crate) fn check_program_not_relative(program: &str) -> Result<(), String> {
    let path = Path::new(program);
    if path.is_absolute() || !program.contains('/') {
        return Ok(());
    }
    Err(format!(
        "`{program}` is a relative path; write an absolute path, or a bare name to look up on PATH. A relative one resolves against whatever directory the chat is in when the hook runs"
    ))
}

#[cfg(test)]
mod tests;
