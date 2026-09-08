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
mod tests {
    use super::*;

    /// A subject standing in for one `execute`-shaped tool call.
    fn tool_input(tool: &str) -> MatchInput<'_> {
        MatchInput::new(HookSubject::tool(tool, "{}"), Path::new("/repo"))
    }

    /// One call with real arguments, resolved against `/repo`.
    fn call_input<'a>(tool: &'a str, arguments: &'a str) -> MatchInput<'a> {
        MatchInput::new(HookSubject::tool(tool, arguments), Path::new("/repo"))
    }

    /// An event that carries no tool at all (`user-prompt-submit`).
    fn no_tool_input() -> MatchInput<'static> {
        MatchInput::new(HookSubject::none(), Path::new("/repo"))
    }

    fn one(command: &str) -> String {
        format!(
            r#"{{"version":1,"hooks":[{{"id":"audit","events":["pre-tool-use"],"command":{command}}}]}}"#
        )
    }

    #[test]
    fn parses_a_minimal_hook_definition() {
        let hooks = parse(&one(r#"["/opt/dsh-hooks/hook.sh"]"#)).unwrap();
        let matched = hooks.matching(HookEvent::PreToolUse, &tool_input("execute"));
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
                .matching(HookEvent::PreToolUse, &tool_input("execute"))
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
                .matching(HookEvent::PreToolUse, &tool_input("mcp__x__y"))
                .len(),
            1
        );
        assert_eq!(
            hooks
                .matching(HookEvent::PreToolUse, &tool_input("edit"))
                .len(),
            1
        );
        assert!(
            hooks
                .matching(HookEvent::PreToolUse, &tool_input("execute"))
                .is_empty()
        );
        // No tool at all cannot satisfy a tool matcher.
        assert!(
            hooks
                .matching(HookEvent::PreToolUse, &no_tool_input())
                .is_empty()
        );
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
                .matching(HookEvent::PreToolUse, &tool_input("mcp__server__tool"))
                .len(),
            1
        );
    }

    /// `{"tools":["execute"]}` is every command; these are the narrowings.
    fn matcher(extra: &str) -> String {
        format!(
            r#"{{"version":1,"hooks":[{{"id":"audit","events":["pre-tool-use"],
               "match":{{{extra}}},"command":["/opt/dsh-hooks/hook.sh"]}}]}}"#
        )
    }

    fn fires(config: &str, tool: &str, arguments: &str) -> bool {
        let hooks = parse(config).expect(config);
        !hooks
            .matching(HookEvent::PreToolUse, &call_input(tool, arguments))
            .is_empty()
    }

    fn command_fires(config: &str, command: &str) -> bool {
        let arguments = serde_json::json!({ "command": command }).to_string();
        fires(config, "execute", &arguments)
    }

    #[test]
    fn programs_matches_through_a_wrapper_and_every_stage() {
        let config = matcher(r#""tools":["execute"],"programs":["rm"]"#);
        for command in [
            "rm -rf /tmp/x",
            "sudo rm -rf /tmp/x",
            "timeout 5 rm /tmp/x",
            "echo hi | rm -rf /tmp/x",
            "cd /tmp && rm -rf x",
        ] {
            assert!(command_fires(&config, command), "{command}");
        }
        for command in ["ls -la", "cargo test", "echo rm"] {
            assert!(!command_fires(&config, command), "{command}");
        }
    }

    #[test]
    fn programs_is_a_word_prefix_not_a_substring() {
        let config = matcher(r#""tools":["execute"],"programs":["git push"]"#);
        assert!(command_fires(&config, "git push --force"));
        assert!(!command_fires(&config, "git pushx"));
        assert!(!command_fires(&config, "git status"));
    }

    /// Dead configuration that looks like a filter is the failure mode here.
    #[test]
    fn programs_without_an_execute_tool_entry_is_a_load_error() {
        let err = parse(&matcher(r#""tools":["edit"],"programs":["rm"]"#)).unwrap_err();
        assert!(err.contains("`programs` needs"), "{err}");

        // A trailing-star pattern that covers `execute` is fine.
        assert!(parse(&matcher(r#""tools":["exec*"],"programs":["rm"]"#)).is_ok());
    }

    #[test]
    fn an_untokenizable_programs_entry_is_a_load_error() {
        let err = parse(&matcher(r#""tools":["execute"],"programs":["'unclosed"]"#)).unwrap_err();
        assert!(err.contains("is not a command"), "{err}");
    }

    /// `{"tools": []}` has always meant "every call". Refusing it would refuse
    /// the whole chat for a configuration that worked yesterday.
    #[test]
    fn an_empty_match_object_loads_and_runs_on_every_call() {
        for extra in ["", r#""tools":[]"#] {
            let config = matcher(extra);
            assert!(command_fires(&config, "anything at all"), "{extra}");
            let hooks = parse(&config).expect(extra);
            assert!(hooks.all()[0].matcher_narrows_nothing(), "{extra}");
        }

        // A matcher that does narrow is not reported as narrowing nothing.
        let narrow = parse(&matcher(r#""tools":["execute"]"#)).unwrap();
        assert!(!narrow.all()[0].matcher_narrows_nothing());
    }

    #[test]
    fn paths_glob_matches_a_path_argument() {
        let config = matcher(r#""tools":["edit"],"paths":["/etc/**"]"#);
        assert!(fires(&config, "edit", r#"{"path":"/etc/hosts"}"#));
        assert!(!fires(&config, "edit", r#"{"path":"/tmp/hosts"}"#));
    }

    /// A relative argument has to be judged where it will land, not as written.
    #[test]
    fn paths_matches_the_lexically_absolute_form() {
        let config = matcher(r#""tools":["read_file"],"paths":["/repo/src/**"]"#);
        assert!(fires(&config, "read_file", r#"{"path":"src/a.rs"}"#));
    }

    #[test]
    fn paths_does_not_let_dot_dot_dodge_the_glob() {
        let config = matcher(r#""tools":["read_file"],"paths":["/etc/**"]"#);
        assert!(fires(
            &config,
            "read_file",
            r#"{"path":"sub/../../etc/shadow"}"#
        ));
    }

    /// Every token, not just the program: `bash <path>/run.sh` names a path.
    #[test]
    fn paths_matches_a_token_of_an_execute_command() {
        let config = matcher(r#""tools":["execute"],"paths":["/etc/**"]"#);
        assert!(command_fires(&config, "cat /etc/shadow"));
        assert!(command_fires(&config, "bash /etc/init.d/thing"));
        assert!(!command_fires(&config, "cat /tmp/shadow"));
    }

    /// Inside a command line a bare word could be a program or a file, and the
    /// two are not distinguishable. Every ambiguity in this module resolves
    /// toward firing, because a needless hook run costs a hook run while a
    /// missed one costs the check.
    #[test]
    fn a_bare_filename_argument_still_reaches_a_paths_matcher() {
        let config = matcher(r#""tools":["execute"],"paths":["**/*.env"]"#);
        assert!(command_fires(&config, "rm .env"));
        assert!(command_fires(&config, "rm ./.env"));
        assert!(command_fires(&config, "cat /repo/.env"));
        assert!(!command_fires(&config, "rm notes.md"));

        // The cost of that choice: a broad pattern is true of a program name
        // too. Specific patterns are the useful ones.
        let broad = matcher(r#""tools":["execute"],"paths":["/repo/**"]"#);
        assert!(command_fires(&broad, "cat"));

        // An option is still not a path; a real one is written `./-foo`.
        let dashes = matcher(r#""tools":["execute"],"paths":["/repo/-p"]"#);
        assert!(!command_fires(&dashes, "cargo test -p"));
    }

    /// `execute` runs the command in its `cwd` argument, so that is where a
    /// relative token lands - the hole `touches_skill_file` closed for skill
    /// scripts, in the matcher this time.
    #[test]
    fn paths_resolve_against_the_calls_own_cwd() {
        let config = matcher(r#""tools":["execute"],"paths":["/etc/sub/**"]"#);
        assert!(fires(
            &config,
            "execute",
            r#"{"command":"cat sub/secret","cwd":"/etc"}"#
        ));
        // Without the `cwd` argument the shell's directory is the base.
        assert!(!fires(
            &config,
            "execute",
            r#"{"command":"cat sub/secret"}"#
        ));
    }

    /// A provider that omits `arguments` for a no-argument tool sends `""`.
    /// Treating that as unreadable made every argument matcher fire on it.
    #[test]
    fn an_absent_arguments_string_is_missing_not_unreadable() {
        let config = matcher(r#""tools":["execute"],"programs":["rm"]"#);
        assert!(!fires(&config, "execute", ""));
        assert!(!fires(&config, "execute", "   "));
        // Text that is actually present and unparseable still fires.
        assert!(fires(&config, "execute", "{not json"));
    }

    /// An MCP tool is as likely to carry paths in a list or a nested object.
    #[test]
    fn paths_are_inferred_from_nested_and_repeated_fields() {
        let config = matcher(r#""tools":["mcp__*"],"paths":["/etc/**"]"#);
        for arguments in [
            r#"{"paths":["/tmp/a","/etc/myservice.conf"]}"#,
            r#"{"target":{"path":"/etc/myservice.conf"}}"#,
            r#"{"jobs":[{"src":"/etc/myservice.conf","dst":"/tmp/x"}]}"#,
        ] {
            assert!(fires(&config, "mcp__fs__write", arguments), "{arguments}");
        }
        assert!(!fires(
            &config,
            "mcp__fs__write",
            r#"{"target":{"path":"/tmp/x"}}"#
        ));
    }

    /// `skill_manage`'s `file` is relative to the skill directory, so joining
    /// it with the chat's cwd tests a path the call never touches.
    #[test]
    fn skill_manage_offers_no_path_candidates() {
        let config = matcher(r#""tools":["skill_manage"],"paths":["/repo/**"]"#);
        assert!(!fires(
            &config,
            "skill_manage",
            r#"{"action":"write_file","name":"deploy","scope":"project","file":"scripts/run.sh"}"#
        ));
    }

    #[test]
    fn paths_matches_the_execute_cwd_argument() {
        let config = matcher(r#""tools":["execute"],"paths":["/etc/**"]"#);
        assert!(fires(
            &config,
            "execute",
            r#"{"command":"ls","cwd":"/etc/apache2"}"#
        ));
    }

    /// No table for an MCP server's arguments; path *shape* stands in.
    #[test]
    fn paths_infers_a_path_shaped_field_of_an_unknown_tool() {
        let config = matcher(r#""tools":["mcp__*"],"paths":["/etc/**"]"#);
        assert!(fires(
            &config,
            "mcp__fs__write",
            r#"{"target":"/etc/myservice.conf","mode":"append"}"#
        ));
        assert!(!fires(
            &config,
            "mcp__fs__write",
            r#"{"target":"notapath","mode":"append"}"#
        ));
    }

    #[test]
    fn paths_with_an_invalid_glob_is_a_load_error() {
        let err = parse(&matcher(r#""tools":["edit"],"paths":["/etc/[bad"]"#)).unwrap_err();
        assert!(err.contains("is not a glob"), "{err}");
    }

    #[test]
    fn arguments_match_is_exact_equality() {
        let config = matcher(r#""tools":["search"],"arguments":{"type":"content"}"#);
        assert!(fires(
            &config,
            "search",
            r#"{"query":"x","type":"content"}"#
        ));
        assert!(!fires(
            &config,
            "search",
            r#"{"query":"x","type":"filename"}"#
        ));
        assert!(!fires(&config, "search", r#"{"query":"x"}"#));

        let numeric = matcher(r#""tools":["read_file"],"arguments":{"offset":1}"#);
        assert!(fires(&numeric, "read_file", r#"{"path":"a","offset":1}"#));
        assert!(!fires(&numeric, "read_file", r#"{"path":"a","offset":2}"#));
    }

    #[test]
    fn an_arguments_entry_that_is_not_a_scalar_is_a_load_error() {
        let err = parse(&matcher(r#""tools":["edit"],"arguments":{"path":["a"]}"#)).unwrap_err();
        assert!(err.contains("must be a string, number or boolean"), "{err}");
    }

    /// Kinds are ANDed; each one alone is not enough.
    #[test]
    fn matcher_kinds_are_anded() {
        let config = matcher(r#""tools":["execute"],"programs":["rm"],"paths":["/etc/**"]"#);
        assert!(!command_fires(&config, "rm -rf /tmp/x"));
        assert!(!command_fires(&config, "cat /etc/shadow"));
        assert!(command_fires(&config, "rm -rf /etc/thing"));
    }

    /// The masked form of this command is `mkdir -p ***`, which no `/etc/**`
    /// matcher can see. Matching therefore reads the unmasked arguments; only
    /// the payload the hook receives is masked.
    #[test]
    fn matching_uses_the_unmasked_arguments() {
        let config = matcher(r#""tools":["execute"],"paths":["/etc/**"]"#);
        let command = "mkdir -p /etc/myapp";
        assert!(
            crate::safety_policy::redact_sensitive_text(command).contains("***"),
            "this test is only meaningful while `-p` is masked"
        );
        assert!(command_fires(&config, command));
    }

    /// A mismatched quote must not be a way to skip a check.
    #[test]
    fn unreadable_arguments_make_an_argument_matcher_fire() {
        for extra in [
            r#""tools":["execute"],"programs":["rm"]"#,
            r#""tools":["execute"],"paths":["/etc/**"]"#,
        ] {
            let config = matcher(extra);
            // A command line that does not tokenise.
            assert!(command_fires(&config, "echo 'unclosed"), "{extra}");
            // Arguments that are not JSON at all.
            assert!(fires(&config, "execute", "{not json"), "{extra}");
        }
        let args = matcher(r#""tools":["search"],"arguments":{"type":"content"}"#);
        assert!(fires(&args, "search", "{not json"));
    }

    /// Structural absence is the other direction: nothing to be true of.
    #[test]
    fn an_argument_matcher_cannot_be_satisfied_without_arguments() {
        for extra in [
            r#""tools":["execute"],"programs":["rm"]"#,
            r#""tools":["execute"],"paths":["/etc/**"]"#,
            r#""tools":["execute"],"arguments":{"command":"rm"}"#,
        ] {
            let hooks = parse(&matcher(extra)).expect(extra);
            assert!(
                hooks
                    .matching(HookEvent::PreToolUse, &no_tool_input())
                    .is_empty(),
                "{extra}"
            );
        }
    }

    /// The shape every existing configuration has.
    #[test]
    fn a_tools_only_config_still_loads() {
        let config = matcher(r#""tools":["execute"]"#);
        assert!(command_fires(&config, "anything at all"));
    }

    /// The per-event cap has to see every event, including the next one added.
    #[test]
    fn every_event_is_covered_by_the_hook_cap() {
        for event in HookEvent::ALL {
            let name = event.as_str();
            let hooks: Vec<String> = (0..=MAX_HOOKS_PER_EVENT)
                .map(|i| {
                    format!(
                        r#"{{"id":"h{i}","events":["{name}"],"command":["/opt/dsh-hooks/hook.sh"]}}"#
                    )
                })
                .collect();
            let config = format!(r#"{{"version":1,"hooks":[{}]}}"#, hooks.join(","));
            let err = parse(&config).unwrap_err();
            assert!(err.contains(name), "{name}: {err}");
        }
    }

    /// Gate-ness and context-carrying are properties of every event, so assert
    /// them over `ALL` rather than over a list that drifts.
    #[test]
    fn every_events_gate_and_context_answers_are_pinned() {
        for event in HookEvent::ALL {
            let expect_gate = matches!(event, HookEvent::UserPromptSubmit | HookEvent::PreToolUse);
            assert_eq!(event.is_gate(), expect_gate, "{}", event.as_str());

            let expect_context = matches!(
                event,
                HookEvent::UserPromptSubmit | HookEvent::PreToolUse | HookEvent::PostToolUse
            );
            assert_eq!(event.uses_context(), expect_context, "{}", event.as_str());
        }
    }

    /// Compaction cannot be refused: the request would just be too large.
    #[test]
    fn pre_compact_neither_gates_nor_takes_context() {
        assert!(!HookEvent::PreCompact.is_gate());
        assert!(!HookEvent::PreCompact.uses_context());
        let hooks = parse(
            r#"{"version":1,"hooks":[{"id":"c","events":["pre-compact"],
               "command":["/opt/dsh-hooks/hook.sh"]}]}"#,
        )
        .unwrap();
        assert_eq!(
            hooks
                .matching(HookEvent::PreCompact, &no_tool_input())
                .len(),
            1
        );
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
