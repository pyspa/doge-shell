//! Loading `hooks.json` from disk: the on-disk shape (`HooksFile`), the mtime+size cache keyed by `FileSignature` (`load`), and the permission/parse checks a fresh read runs (`read`/`check_permissions`/`parse`).
use super::*;

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
pub(super) fn deserialize_command<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
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
    match crate::chatgpt::resolve_setting(proxy, HOOKS_ENABLED_KEY) {
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
    let Some(raw) = crate::chatgpt::resolve_setting(proxy, HOOK_TURN_BUDGET_KEY) else {
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
    if let Some(path) = crate::chatgpt::resolve_setting(proxy, HOOKS_CONFIG_KEY) {
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
        return if crate::chatgpt::resolve_setting(proxy, HOOKS_CONFIG_KEY).is_some() {
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
        return pin_loaded(proxy, cached);
    }

    let hooks = read(&path)?;

    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((signature, hooks.clone()));
    }

    pin_loaded(proxy, hooks)
}

/// Pin cached/parsed hooks against the proxy's logical runtime snapshot.
///
/// The cache stores parsed-but-unpinned hooks (keyed by file signature
/// only), so every `load` pins afresh: a `PATH` change must move the next
/// turn's pinning without waiting for a file edit.
fn pin_loaded(proxy: &mut dyn ShellProxy, mut hooks: LoadedHooks) -> Result<LoadedHooks, String> {
    let snapshot = proxy
        .command_runtime_snapshot()
        .map_err(|err| format!("chat: cannot snapshot runtime for hooks: {err:#}"))?;
    for hook in &mut hooks.hooks {
        let program = hook.command[0].clone();
        hook.command[0] = super::pin_program(&snapshot, &program)
            .map_err(|err| format!("hook `{}`: {err}", hook.id))?;
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
        // Relative pathnames are refused here (no runtime needed); bare
        // names are pinned against the logical runtime later in `load`, so
        // a later `chdir` cannot reinterpret them.
        super::check_program_not_relative(&hook.command[0])
            .map_err(|err| format!("hook `{}`: {err}", hook.id))?;
    }

    Ok(LoadedHooks { hooks })
}
