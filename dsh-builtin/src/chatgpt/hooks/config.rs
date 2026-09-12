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
/// `dsh` funnels every command through the one `execute` tool, so
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

    pub(crate) fn matching(
        &self,
        event: HookEvent,
        input: &MatchInput<'_>,
    ) -> Vec<&HookDefinition> {
        self.hooks
            .iter()
            .filter(|hook| hook.enabled && hook.events.contains(&event) && hook.matches(input))
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

/// The turn-wide hook time budget, or `None` for unlimited.
///
/// `0` means unlimited too, matching `AI_CHAT_SESSION_TTL_SECS`. A value below
/// `MIN_TIMEOUT_MS` would let no hook run to completion, so it is refused
/// rather than silently disabling every hook.
pub(crate) fn turn_budget_ms(proxy: &mut dyn ShellProxy) -> Result<Option<u64>, String> {
    let Some(raw) = super::super::resolve_setting(proxy, HOOK_TURN_BUDGET_KEY) else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value: u64 = trimmed.parse().map_err(|_| {
        format!("chat: {HOOK_TURN_BUDGET_KEY} must be a whole number of milliseconds, got `{raw}`")
    })?;
    if value == 0 {
        return Ok(None);
    }
    if value < MIN_TIMEOUT_MS {
        return Err(format!(
            "chat: {HOOK_TURN_BUDGET_KEY} of {value}ms is below the {MIN_TIMEOUT_MS}ms minimum \
             one hook needs; use 0 to remove the budget"
        ));
    }
    Ok(Some(value))
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
        if let Some(matcher) = &hook.matcher {
            if let Some(bad) = matcher.tools.iter().find(|pattern| {
                // `trim_end_matches` strips *every* trailing star, so `mcp**`
                // passed validation and then matched nothing at all - the
                // silent no-op this module exists to prevent.
                pattern.strip_suffix('*').unwrap_or(pattern).contains('*')
            }) {
                return Err(format!(
                    "hook `{}`: `{bad}` may only use `*` as the last character",
                    hook.id
                ));
            }
            validate_matcher(&hook.id, matcher)?;
        }
    }

    for event in HookEvent::ALL {
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
mod tests;
