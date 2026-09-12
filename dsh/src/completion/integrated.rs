use super::cache::CompletionCache;
use super::command::{
    ArgumentType, CommandCompletionDatabase, CommandOption, CompletionCandidate, SubCommand,
};
use super::context::ContextCorrector;
use super::dynamic::{CachePolicy, CompletionRuntime, DynamicCompletionProvider};

use super::framework::CompletionFrameworkKind;

use super::generator::CompletionGenerator;
use super::shell_token::{self, SeparatorMode};
use crate::completion::generators::filesystem::FileSystemGenerator;

use super::json_loader::JsonCompletionLoader;
use super::parser::{self, CommandLineParser, ParsedCommandLine};
use crate::completion::display::Candidate;
use crate::completion::generators::argument::ArgumentGenerator;
use crate::environment::Environment;
use anyhow::Result;
use dsh_builtin::project;
use dsh_types::mcp::McpTransport;
use parking_lot::{Mutex, RwLock};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

const DEFAULT_CACHE_TTL_MS: u64 = 3000;
const HISTORY_BOOST_SCAN_LIMIT: usize = 512;
/// How long "this command has no JSON completion definition" is trusted. Long
/// enough that typing never re-stats the same name, short enough that a
/// definition written mid-session (`comp-gen`) starts working without a restart.
const MISSING_COMPLETION_TTL: Duration = Duration::from_secs(30);
/// How many `CommandWithArgs` wrappers (`sudo env pacman ...`) are peeled off
/// before giving up. Deeper nesting is not realistic at a prompt.
const MAX_COMMAND_WRAPPER_DEPTH: usize = 3;
const HISTORY_BOOST_SCORE_CAP: u32 = 5000;
const JS_TASK_SOURCES: &[&str] = &["npm", "pnpm", "yarn", "bun"];
const DENO_TASK_SOURCES: &[&str] = &["deno"];
const JUST_TASK_SOURCES: &[&str] = &["just"];
const MAKE_TASK_SOURCES: &[&str] = &["make"];

type DynamicProviderFn = for<'a> fn(
    &IntegratedCompletionEngine,
    &CompletionRequest<'a>,
    &ParsedCommandLine,
    CachePolicy,
) -> Vec<EnhancedCandidate>;

struct DynamicProviderSpec {
    command: &'static str,
    collect: DynamicProviderFn,
}

static COMPLETION_STAGE_TIMING_ENABLED: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("DSH_COMPLETION_TIMING")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
});

const DYNAMIC_PROVIDER_SPECS: &[DynamicProviderSpec] = &[
    DynamicProviderSpec {
        command: "task",
        collect: collect_task_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pm",
        collect: collect_pm_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pj",
        collect: collect_pj_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "mcp",
        collect: collect_mcp_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "skill",
        collect: collect_skill_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "git",
        collect: collect_git_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "docker",
        collect: collect_docker_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "kubectl",
        collect: collect_kubectl_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "cargo",
        collect: collect_cargo_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "systemctl",
        collect: collect_systemctl_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "journalctl",
        collect: collect_journalctl_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "ssh",
        collect: collect_ssh_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "scp",
        collect: collect_scp_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "rsync",
        collect: collect_rsync_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "tmux",
        collect: collect_tmux_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "screen",
        collect: collect_screen_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pgrep",
        collect: collect_pgrep_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pkill",
        collect: collect_pkill_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pip",
        collect: collect_pip_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pip3",
        collect: collect_pip3_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "rustup",
        collect: collect_rustup_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "gh",
        collect: collect_gh_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "nmcli",
        collect: collect_nmcli_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pacman",
        collect: collect_pacman_dynamic_candidates,
    },
    // AUR helpers take the same operation flags and share the pacman package
    // provider in their JSON definitions, so they need the same bundled-flag
    // handling (`yay -Rns`, `paru -Syu`).
    DynamicProviderSpec {
        command: "yay",
        collect: collect_pacman_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "paru",
        collect: collect_pacman_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "mount",
        collect: collect_mount_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "umount",
        collect: collect_umount_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "modprobe",
        collect: collect_modprobe_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "tcpdump",
        collect: collect_tcpdump_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "npm",
        collect: collect_npm_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "pnpm",
        collect: collect_npm_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "yarn",
        collect: collect_yarn_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "deno",
        collect: collect_deno_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "just",
        collect: collect_just_dynamic_candidates,
    },
    DynamicProviderSpec {
        command: "make",
        collect: collect_make_dynamic_candidates,
    },
];

#[derive(Debug, Clone, Copy)]
struct CompletionRequest<'a> {
    input: &'a str,
    current_dir: &'a Path,
    max_results: usize,
    cursor_pos: usize,
}

struct CompletionTiming {
    enabled: bool,
    last: Instant,
    stages: Vec<(&'static str, Duration)>,
}

impl CompletionTiming {
    fn start() -> Self {
        let now = Instant::now();
        Self {
            enabled: *COMPLETION_STAGE_TIMING_ENABLED,
            last: now,
            stages: Vec::new(),
        }
    }

    fn mark(&mut self, stage: &'static str) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        self.stages.push((stage, now.duration_since(self.last)));
        self.last = now;
    }

    fn finish(mut self, input: &str, outcome: &'static str) {
        if !self.enabled {
            return;
        }
        self.mark(outcome);
        let summary = self
            .stages
            .into_iter()
            .map(|(stage, elapsed)| format!("{stage}={}us", elapsed.as_micros()))
            .collect::<Vec<_>>()
            .join(" ");
        debug!("completion timing input={input:?} {summary}");
    }
}

impl<'a> CompletionRequest<'a> {
    fn new(input: &'a str, current_dir: &'a Path, max_results: usize, cursor_pos: usize) -> Self {
        Self {
            input,
            current_dir,
            max_results,
            cursor_pos,
        }
    }
}

#[derive(Debug, Default)]
struct CandidateBatch {
    candidates: Vec<EnhancedCandidate>,
    exclusive: bool,
    framework: Option<CompletionFrameworkKind>,
}

impl CandidateBatch {
    fn empty() -> Self {
        Self {
            candidates: Vec::new(),
            exclusive: false,
            framework: None,
        }
    }

    fn inclusive_with_framework(
        candidates: Vec<EnhancedCandidate>,
        framework: CompletionFrameworkKind,
    ) -> Self {
        Self {
            candidates,
            exclusive: false,
            framework: Some(framework),
        }
    }

    fn exclusive_with_framework(
        candidates: Vec<EnhancedCandidate>,
        framework: CompletionFrameworkKind,
    ) -> Self {
        Self {
            candidates,
            exclusive: true,
            framework: Some(framework),
        }
    }
}

#[derive(Debug)]
struct CommandCollection {
    batch: CandidateBatch,
}

impl CommandCollection {
    fn empty() -> Self {
        Self {
            batch: CandidateBatch::empty(),
        }
    }
}

#[derive(Debug, Clone)]
struct ParsedCommandLineCache {
    input: String,
    cursor_pos: usize,
    parsed: ParsedCommandLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionReplacementRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub struct CompletionResult {
    pub candidates: Vec<EnhancedCandidate>,
    pub framework: CompletionFrameworkKind,
    pub replacement_range: Option<CompletionReplacementRange>,
}

struct CandidateAggregator<'a> {
    engine: &'a IntegratedCompletionEngine,
    max_results: usize,
    collected: Vec<EnhancedCandidate>,
    framework: Option<CompletionFrameworkKind>,
    command_context: Option<String>,
}

impl<'a> CandidateAggregator<'a> {
    fn new(
        engine: &'a IntegratedCompletionEngine,
        max_results: usize,
        command_context: Option<String>,
    ) -> Self {
        Self {
            engine,
            max_results,
            collected: Vec::new(),
            framework: None,
            command_context,
        }
    }

    fn extend(&mut self, batch: CandidateBatch) -> bool {
        if batch.candidates.is_empty() {
            return !batch.exclusive;
        }

        debug!(
            "Aggregating {} candidates (exclusive: {})",
            batch.candidates.len(),
            batch.exclusive
        );
        self.collected.extend(batch.candidates);
        if self.framework.is_none() {
            self.framework = batch.framework;
        }
        !batch.exclusive
    }

    fn finalize(
        self,
        history: Option<&Arc<parking_lot::Mutex<crate::history::History>>>,
    ) -> CompletionResult {
        let candidates = self.engine.deduplicate_and_sort(
            self.collected,
            self.max_results,
            history,
            self.command_context.as_deref(),
        );

        // Determine framework based on candidate types:
        // - If all candidates are File or Directory, use Inline
        // - Otherwise, use the batch framework or default
        let all_file_or_dir = !candidates.is_empty()
            && candidates.iter().all(|c| {
                matches!(
                    c.candidate_type,
                    CandidateType::File | CandidateType::Directory
                )
            });

        let framework = if all_file_or_dir {
            CompletionFrameworkKind::Inline
        } else {
            self.framework
                .unwrap_or_else(super::default_completion_framework)
        };

        CompletionResult {
            candidates,
            framework,
            replacement_range: None,
        }
    }
}

/// Integrated completion engine - integrates all completion features
pub struct IntegratedCompletionEngine {
    /// JSON-based command completion
    command_completion: Arc<Mutex<CommandCompletionDatabase>>,
    loader: Option<JsonCompletionLoader>,
    /// Commands recently found to have no JSON definition, so the loader is not
    /// re-run for them on every keystroke. TTL-bounded so a definition created
    /// mid-session (e.g. by `comp-gen`) is still picked up.
    missing_command_completions: RwLock<HashMap<String, Instant>>,
    /// Command line parser
    parser: CommandLineParser,
    parsed_cache: RwLock<Option<ParsedCommandLineCache>>,

    /// Dynamic completion registry

    /// Short lived completion cache
    cache: CompletionCache<EnhancedCandidate>,
    framework_cache: RwLock<HashMap<String, CompletionFrameworkKind>>,
    shell_jobs: RwLock<Vec<(usize, String, String)>>,
    dynamic: DynamicCompletionProvider,
    runtime: Arc<CompletionRuntime>,

    /// Shell environment (for dynamic completion)
    pub environment: Arc<RwLock<Environment>>,
}

impl IntegratedCompletionEngine {
    /// Create a new integrated completion engine
    pub fn new(environment: Arc<RwLock<Environment>>) -> Self {
        let runtime = Arc::new(CompletionRuntime::new());
        Self {
            command_completion: Arc::new(Mutex::new(CommandCompletionDatabase::new())),
            loader: None,
            missing_command_completions: RwLock::new(HashMap::new()),
            parser: CommandLineParser::new(),
            parsed_cache: RwLock::new(None),

            cache: CompletionCache::new(Duration::from_millis(DEFAULT_CACHE_TTL_MS)),
            framework_cache: RwLock::new(HashMap::new()),
            shell_jobs: RwLock::new(Vec::new()),
            dynamic: DynamicCompletionProvider::with_runtime(environment.clone(), runtime.clone()),
            runtime,
            environment,
        }
    }

    pub(crate) fn set_notifier(&self, sender: tokio::sync::mpsc::UnboundedSender<()>) {
        self.runtime.set_notifier(sender);
    }

    pub(crate) fn runtime(&self) -> Arc<CompletionRuntime> {
        self.runtime.clone()
    }

    pub(crate) fn set_shell_jobs(&self, jobs: Vec<(usize, String, String)>) {
        let mut current = self.shell_jobs.write();
        if *current == jobs {
            return;
        }
        *current = jobs;
        self.cache.clear();
        self.framework_cache.write().clear();
    }

    /// Initialize the command completion database
    /// This now sets up the loader but does not eagerly load everything
    pub fn initialize_command_completion(&mut self) -> Result<()> {
        let loader = JsonCompletionLoader::new();
        // We start with an empty database and load on demand
        self.loader = Some(loader);
        Ok(())
    }

    /// Convert ParsedCommand to ParsedCommandLine for dynamic completion
    fn convert_to_parsed_command_line(&self, input: &str, cursor_pos: usize) -> ParsedCommandLine {
        if let Some(cached) = self.parsed_cache.read().as_ref()
            && cached.input == input
            && cached.cursor_pos == cursor_pos
        {
            return cached.parsed.clone();
        }

        let mut parsed = self.parser.parse(input, cursor_pos);
        self.normalize_parsed_command_line(&mut parsed);

        *self.parsed_cache.write() = Some(ParsedCommandLineCache {
            input: input.to_string(),
            cursor_pos,
            parsed: parsed.clone(),
        });

        parsed
    }

    /// Resolve the alias, load the command definition and apply context
    /// correction to a freshly parsed line.
    ///
    /// Shared by the top-level parse and by wrapper unwrapping so an inner
    /// command line (`pacman -R` inside `sudo pacman -R`) is normalized exactly
    /// like a line typed on its own.
    fn normalize_parsed_command_line(&self, parsed: &mut ParsedCommandLine) {
        parsed.command = self.environment.read().resolve_alias(&parsed.command);

        self.ensure_command_completion_loaded(&parsed.command);
        {
            let db_lock = self.command_completion.lock();
            if db_lock.get_command(&parsed.command).is_some() {
                *parsed = CompletionGenerator::new(&db_lock).correct_parsed_command_line(parsed);
            }
        }

        // Update args to use specified_arguments and options to use specified_options
        parsed.args = parsed.specified_arguments.clone();
        parsed.options = parsed.specified_options.clone();
    }

    fn ensure_command_completion_loaded(&self, command_name: &str) {
        if command_name.is_empty() {
            return;
        }

        let Some(loader) = &self.loader else {
            return;
        };

        // The lock is released before the loader runs: `load_command_completion`
        // stats several directories, and every keystroke reaches this method.
        {
            let db = self.command_completion.lock();
            if db.get_command(command_name).is_some() {
                return;
            }
        }

        // Most command names typed at a prompt have no JSON definition — that
        // includes every prefix of a name as it is being typed — so without this
        // the same directory stats are repeated on every keystroke, forever.
        if let Some(checked_at) = self.missing_command_completions.read().get(command_name)
            && checked_at.elapsed() < MISSING_COMPLETION_TTL
        {
            return;
        }

        debug!("Lazy loading completion for command: {}", command_name);
        match loader.load_command_completion(command_name) {
            Ok(Some(completion)) => {
                self.missing_command_completions
                    .write()
                    .remove(command_name);
                self.command_completion.lock().add_command(completion);
            }
            Ok(None) => {
                debug!("No completion definition found for {}", command_name);
                self.remember_missing_command_completion(command_name);
            }
            Err(e) => {
                warn!("Failed to load completion for {}: {}", command_name, e);
                self.remember_missing_command_completion(command_name);
            }
        }
    }

    fn remember_missing_command_completion(&self, command_name: &str) {
        let now = Instant::now();
        let mut missing = self.missing_command_completions.write();
        missing.retain(|_, checked_at| now.duration_since(*checked_at) < MISSING_COMPLETION_TTL);
        missing.insert(command_name.to_string(), now);
    }

    /// Execute integrated completion
    pub async fn complete(
        &self,
        input: &str,
        cursor_pos: usize,
        current_dir: &Path,
        max_results: usize,
        history: Option<&Arc<parking_lot::Mutex<crate::history::History>>>,
    ) -> CompletionResult {
        debug!(
            "Integrated completion for: '{}' at position {} in {:?}",
            input, cursor_pos, current_dir
        );

        let request = CompletionRequest::new(input, current_dir, max_results, cursor_pos);
        let mut timing = CompletionTiming::start();

        let parsed_command_line = self.convert_to_parsed_command_line(input, cursor_pos);
        let replacement_range =
            completion_replacement_range(input, cursor_pos, &parsed_command_line);
        let dynamic_generation = self.dynamic.refresh_generation();
        let mut cache_allowed = completion_cache_allowed(&parsed_command_line)
            && !self.dynamic.has_async_fallback()
            && !self.dynamic.has_pending_refresh();
        timing.mark("parse");

        // Variable ($VAR / ${VAR) and user-home (~user) references are completed
        // uniformly across all commands, independent of per-command definitions.
        // Handled before the cache so freshly `export`ed variables show up
        // immediately (enumeration is cheap: no subprocess, just map lookups).
        if let Some(candidates) =
            self.collect_special_token_candidates(&parsed_command_line.current_token)
        {
            timing.finish(request.input, "special_token");
            return CompletionResult {
                candidates,
                framework: CompletionFrameworkKind::Skim,
                replacement_range,
            };
        }

        if cache_allowed
            && !request.input.is_empty()
            && let Some(hit) = self.cache.lookup(request.input)
        {
            timing.mark("cache_lookup");
            debug!(
                "cache hit for '{}' (key: '{}', exact: {})",
                request.input, hit.key, hit.exact
            );

            if hit.exact || !hit.candidates.is_empty() {
                self.cache.extend_ttl(&hit.key);
                let framework = self
                    .lookup_cached_framework(&hit.key)
                    .unwrap_or_else(super::default_completion_framework);

                timing.finish(request.input, "cache_hit");
                return CompletionResult {
                    candidates: hit.candidates,
                    framework,
                    replacement_range,
                };
            }
        } else {
            timing.mark("cache_lookup");
        }

        let command_context = if !parsed_command_line.command.is_empty() {
            Some(parsed_command_line.command.clone())
        } else {
            None
        };

        let mut aggregator =
            CandidateAggregator::new(self, request.max_results, command_context.clone());

        // 1. Project-aware dynamic completion
        let dynamic_batch = self.collect_dynamic_candidates(&request, &parsed_command_line);
        cache_allowed = cache_allowed
            && dynamic_generation == self.dynamic.refresh_generation()
            && !self.dynamic.has_pending_refresh();
        timing.mark("dynamic");
        if !aggregator.extend(dynamic_batch) {
            let mut results = aggregator.finalize(history);
            timing.mark("finalize");
            results.replacement_range = replacement_range;
            if cache_allowed {
                self.store_in_cache(request.input, &results.candidates, results.framework);
            }
            timing.finish(request.input, "dynamic_exclusive");
            return results;
        }

        // 2. JSON-based command completion
        let command_collection = self.collect_command_candidates(&request, &parsed_command_line);
        timing.mark("json");
        if !aggregator.extend(command_collection.batch) {
            let mut results = aggregator.finalize(history);
            timing.mark("finalize");
            results.replacement_range = replacement_range;
            if cache_allowed {
                self.store_in_cache(request.input, &results.candidates, results.framework);
            }
            timing.finish(request.input, "json_exclusive");
            return results;
        }

        // 3. External completer fallback
        let external_batch = self.collect_external_candidates(&request, &parsed_command_line);
        timing.mark("external");
        if !aggregator.extend(external_batch) {
            let mut results = aggregator.finalize(history);
            timing.mark("finalize");
            results.replacement_range = replacement_range;
            if cache_allowed {
                self.store_in_cache(request.input, &results.candidates, results.framework);
            }
            timing.finish(request.input, "external_exclusive");
            return results;
        }

        // 4. Optional fish-compatible fallback. This is deliberately lower priority than
        // project-aware dynamic and JSON completion, but it can supply broad fish-style
        // candidates for commands without built-in definitions.
        let fish_batch = self.collect_fish_fallback_candidates(&request, &parsed_command_line);
        timing.mark("fish");
        if !aggregator.extend(fish_batch) {
            let mut results = aggregator.finalize(history);
            timing.mark("finalize");
            results.replacement_range = replacement_range;
            if cache_allowed {
                self.store_in_cache(request.input, &results.candidates, results.framework);
            }
            timing.finish(request.input, "fish_exclusive");
            return results;
        }

        let mut results = aggregator.finalize(history);
        timing.mark("finalize");
        results.replacement_range = replacement_range;
        if cache_allowed {
            self.store_in_cache(request.input, &results.candidates, results.framework);
        }
        timing.finish(request.input, "complete");
        results
    }

    pub fn ghost_completion(
        &self,
        input: &str,
        cursor_pos: usize,
        current_dir: &Path,
        history: Option<&Arc<parking_lot::Mutex<crate::history::History>>>,
    ) -> Option<String> {
        if input.is_empty() || cursor_pos != input.chars().count() {
            return None;
        }

        let request = CompletionRequest::new(input, current_dir, 10, cursor_pos);
        let parsed_command_line = self.convert_to_parsed_command_line(input, cursor_pos);
        let replacement_range =
            completion_replacement_range(input, cursor_pos, &parsed_command_line)?;

        // Variable / user-home references: predict the first matching candidate.
        if let Some(candidates) =
            self.collect_special_token_candidates(&parsed_command_line.current_token)
        {
            let candidate = candidates.first()?;
            let full = replace_char_range(
                input,
                replacement_range.start,
                replacement_range.end,
                &candidate.text,
            );
            if full == input || !full.starts_with(input) {
                return None;
            }
            return Some(full);
        }

        let mut candidates = self.collect_dynamic_candidates_cached(&request, &parsed_command_line);
        candidates.extend(
            self.collect_command_candidates_for_ghost(&parsed_command_line, request.current_dir),
        );

        let command_context = if parsed_command_line.command.is_empty() {
            None
        } else {
            Some(parsed_command_line.command.as_str())
        };

        let candidates = self.deduplicate_and_sort(candidates, 10, history, command_context);
        let candidate = candidates.first()?;
        let full = replace_char_range(
            input,
            replacement_range.start,
            replacement_range.end,
            &candidate.text,
        );

        if full == input || !full.starts_with(input) {
            return None;
        }

        Some(full)
    }

    /// Collect candidates for "special" tokens that complete the same way
    /// regardless of the command: environment/shell variable references
    /// (`$VAR`, `${VAR`) and user-home references (`~user`). Returns `None`
    /// when the current token is not one of these, so normal completion
    /// proceeds unchanged.
    fn collect_special_token_candidates(
        &self,
        current_token: &str,
    ) -> Option<Vec<EnhancedCandidate>> {
        if let Some(rest) = current_token.strip_prefix("${") {
            // Still typing the name inside `${...}` (no closing brace / path yet).
            if rest.contains('}') || rest.contains('/') {
                return None;
            }
            return Some(self.variable_candidates(rest, |name| format!("${{{name}}}")));
        }
        if let Some(rest) = current_token.strip_prefix('$') {
            // `$NAME`. A `/` means it is really a path after expansion, not a
            // bare variable, so leave it to path completion.
            if rest.contains('/') {
                return None;
            }
            return Some(self.variable_candidates(rest, |name| format!("${name}")));
        }
        if let Some(rest) = current_token.strip_prefix('~') {
            // `~user`. Once a `/` appears it is a path under the home directory.
            if rest.contains('/') {
                return None;
            }
            return Some(tilde_user_candidates(rest));
        }
        None
    }

    /// Build variable-name candidates from the shell environment. Names come
    /// from system environment variables, shell-local variables, and the live
    /// process environment, deduplicated and sorted. `format_value` renders the
    /// final replacement text (e.g. `$NAME` or `${NAME}`).
    fn variable_candidates(
        &self,
        prefix: &str,
        format_value: impl Fn(&str) -> String,
    ) -> Vec<EnhancedCandidate> {
        let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        {
            let env = self.environment.read();
            names.extend(env.variable_state.system_env_vars.keys().cloned());
            names.extend(env.variable_state.variables.keys().cloned());
        }
        names.extend(std::env::vars().map(|(key, _)| key));

        names
            .into_iter()
            .filter(|name| prefix.is_empty() || name.starts_with(prefix))
            .map(|name| EnhancedCandidate {
                text: format_value(&name),
                description: Some("environment variable".to_string()),
                candidate_type: CandidateType::Generic,
                priority: 140,
            })
            .collect()
    }

    fn collect_command_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
    ) -> CommandCollection {
        if parsed_command_line.completion_context == parser::CompletionContext::Command {
            debug!("No completion context found - skipping JSON completion");
            return CommandCollection::empty();
        }

        self.ensure_command_completion_loaded(&parsed_command_line.command);

        let db_lock = self.command_completion.lock();

        let completion_generator = CompletionGenerator::new(&db_lock);

        match completion_generator.generate_candidates(parsed_command_line) {
            Ok(command_candidates) => {
                let enhanced_candidates = command_candidates
                    .into_iter()
                    .map(|c| self.convert_to_enhanced_candidate(c))
                    .collect::<Vec<_>>();

                debug!(
                    "JSON completion generated {} candidates for '{}'",
                    enhanced_candidates.len(),
                    request.input
                );

                CommandCollection {
                    batch: CandidateBatch::inclusive_with_framework(
                        enhanced_candidates,
                        CompletionFrameworkKind::Skim,
                    ),
                }
            }
            // Add retry logic for lazy loading of inner commands
            Err(super::generator::GeneratorError::MissingCommand(cmd)) => {
                // Drop lock to load
                drop(db_lock);
                debug!("Generator requested lazy load for command: {}", cmd);

                if let Some(loader) = &self.loader {
                    match loader.load_command_completion(&cmd) {
                        Ok(Some(completion)) => {
                            self.command_completion.lock().add_command(completion);

                            // Retry generation with loaded command
                            let db_lock = self.command_completion.lock();
                            let completion_generator = CompletionGenerator::new(&db_lock);
                            match completion_generator.generate_candidates(parsed_command_line) {
                                Ok(candidates) => {
                                    let enhanced_candidates = candidates
                                        .into_iter()
                                        .map(|c| self.convert_to_enhanced_candidate(c))
                                        .collect::<Vec<_>>();

                                    return CommandCollection {
                                        batch: CandidateBatch::inclusive_with_framework(
                                            enhanced_candidates,
                                            CompletionFrameworkKind::Skim,
                                        ),
                                    };
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to generate JSON completion after lazy load: {}",
                                        e
                                    );
                                }
                            }
                        }
                        Ok(None) => {
                            debug!("No completion definition found for {}", cmd);
                        }
                        Err(e) => {
                            warn!("Failed to load completion for {}: {}", cmd, e);
                        }
                    }
                }

                // Fallback if loading failed or returned nothing
                let db_lock = self.command_completion.lock();
                let completion_generator = CompletionGenerator::new(&db_lock);
                if let Ok(candidates) = completion_generator
                    .generate_fallback_candidates(&parsed_command_line.current_token)
                {
                    let enhanced_candidates = candidates
                        .into_iter()
                        .map(|c| self.convert_to_enhanced_candidate(c))
                        .collect();
                    CommandCollection {
                        batch: CandidateBatch::inclusive_with_framework(
                            enhanced_candidates,
                            CompletionFrameworkKind::Skim,
                        ),
                    }
                } else {
                    CommandCollection {
                        batch: CandidateBatch::empty(),
                    }
                }
            }
            Err(e) => {
                warn!("Failed to generate JSON completion candidates: {}", e);
                CommandCollection {
                    batch: CandidateBatch::empty(),
                }
            }
        }
    }

    /// Peel off one `CommandWithArgs` wrapper (`sudo`, `env`, `nice`, ...) and
    /// return the wrapped command line, or `None` when there is nothing to
    /// unwrap.
    ///
    /// `self.command_completion` is a non-reentrant mutex and
    /// `ensure_command_completion_loaded` takes it, so both loads happen with
    /// the lock released.
    fn unwrap_command_with_args_once(
        &self,
        parsed: &parser::ParsedCommandLine,
    ) -> Option<parser::ParsedCommandLine> {
        self.ensure_command_completion_loaded(&parsed.command);

        let mut inner = {
            let db_lock = self.command_completion.lock();
            let corrector = ContextCorrector::new(&db_lock);
            let (cmd_index, cmd_name) = corrector.find_command_with_args_arg(parsed)?;
            if !cursor_follows_wrapped_command(parsed, &cmd_name) {
                return None;
            }
            corrector.reparse_inner_command(parsed, cmd_index, cmd_name)
        };

        if inner.command.is_empty() || inner.command == parsed.command {
            return None;
        }

        self.normalize_parsed_command_line(&mut inner);
        Some(inner)
    }

    /// The command line the dynamic providers should be keyed off.
    ///
    /// `sudo pacman -R <TAB>` parses with `command == "sudo"`, and every
    /// dynamic provider lookup keys off the command name, so the pacman package
    /// provider never ran and the completion fell through to the fish fallback
    /// (which lists sync-repository packages only, dropping AUR packages).
    /// Unwrapping here makes the wrapped command reach its provider.
    fn dynamic_provider_target<'a>(
        &self,
        parsed: &'a parser::ParsedCommandLine,
    ) -> Cow<'a, parser::ParsedCommandLine> {
        let mut current = Cow::Borrowed(parsed);

        for _ in 0..MAX_COMMAND_WRAPPER_DEPTH {
            let Some(inner) = self.unwrap_command_with_args_once(&current) else {
                break;
            };
            // Each unwrap must consume the wrapper's own tokens. Bail out rather
            // than spin if a malformed definition ever breaks that.
            if inner.raw_args.len() >= current.raw_args.len() {
                break;
            }
            current = Cow::Owned(inner);
        }

        current
    }

    fn collect_dynamic_candidates_for(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let mut candidates = DYNAMIC_PROVIDER_SPECS
            .iter()
            .find(|provider| provider.command == parsed_command_line.command)
            .map(|provider| (provider.collect)(self, request, parsed_command_line, cache_policy))
            .unwrap_or_default();
        candidates.extend(self.collect_declared_dynamic_candidates(
            request,
            parsed_command_line,
            cache_policy,
        ));
        candidates
    }

    fn collect_dynamic_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
    ) -> CandidateBatch {
        let target = self.dynamic_provider_target(parsed_command_line);
        let mut candidates =
            self.collect_dynamic_candidates_for(request, &target, CachePolicy::RefreshInBackground);

        // `git checkout`/`git restore` accept BOTH refs and working-tree paths.
        // The dynamic provider only yields branches, and these subcommands are
        // treated as exclusive (so later file stages never run), which would
        // otherwise make `git checkout <file>` impossible to complete. Merge in
        // file/directory candidates so both branches and paths are offered.
        if git_subcommand_accepts_paths(&target) {
            candidates.extend(self.file_candidates_for_token(&target.current_token));
        }

        if dynamic_candidates_are_exclusive(&target) {
            CandidateBatch::exclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
        } else if candidates.is_empty() {
            CandidateBatch::empty()
        } else {
            CandidateBatch::inclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
        }
    }

    /// File/directory candidates for the current token, converted to the
    /// engine's `EnhancedCandidate` form. Errors are swallowed (completion is
    /// best-effort) and yield an empty list.
    fn file_candidates_for_token(&self, current_token: &str) -> Vec<EnhancedCandidate> {
        match FileSystemGenerator::generate_file_candidates(current_token) {
            Ok(candidates) => candidates
                .into_iter()
                .map(|c| self.convert_to_enhanced_candidate(c))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn collect_declared_dynamic_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
        cache_policy: CachePolicy,
    ) -> Vec<EnhancedCandidate> {
        let Some(ArgumentType::Dynamic { provider, scope }) =
            self.argument_type_for_completion_context(parsed_command_line)
        else {
            return Vec::new();
        };

        if provider == "shell.job" {
            return self.collect_shell_job_candidates(&parsed_command_line.current_token);
        }

        self.dynamic.collect_declared_dynamic_candidates(
            &provider,
            scope.as_deref(),
            parsed_command_line,
            request.current_dir,
            cache_policy,
        )
    }

    fn argument_type_for_completion_context(
        &self,
        parsed_command_line: &parser::ParsedCommandLine,
    ) -> Option<ArgumentType> {
        self.ensure_command_completion_loaded(&parsed_command_line.command);
        let db_lock = self.command_completion.lock();
        argument_type_for_completion_context(&db_lock, parsed_command_line)
    }

    fn collect_dynamic_candidates_cached(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        let target = self.dynamic_provider_target(parsed_command_line);
        self.collect_dynamic_candidates_for(request, &target, CachePolicy::CachedOnly)
    }

    fn collect_command_candidates_for_ghost(
        &self,
        parsed_command_line: &parser::ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        match parsed_command_line.completion_context {
            parser::CompletionContext::SubCommand
            | parser::CompletionContext::ShortOption
            | parser::CompletionContext::LongOption => {}
            parser::CompletionContext::Argument { .. }
            | parser::CompletionContext::OptionValue { .. } => {
                return self
                    .collect_safe_value_candidates_for_ghost(parsed_command_line, current_dir);
            }
            _ => return Vec::new(),
        }

        self.ensure_command_completion_loaded(&parsed_command_line.command);
        let db_lock = self.command_completion.lock();
        if db_lock.get_command(&parsed_command_line.command).is_none() {
            return Vec::new();
        }

        let completion_generator = CompletionGenerator::new(&db_lock);
        match completion_generator.generate_candidates(parsed_command_line) {
            Ok(candidates) => candidates
                .into_iter()
                .map(|candidate| self.convert_to_enhanced_candidate(candidate))
                .collect(),
            Err(err) => {
                debug!("Failed to generate ghost completion candidates: {}", err);
                Vec::new()
            }
        }
    }

    fn collect_safe_value_candidates_for_ghost(
        &self,
        parsed_command_line: &parser::ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        self.ensure_command_completion_loaded(&parsed_command_line.command);
        let db_lock = self.command_completion.lock();
        let Some(arg_type) = argument_type_for_completion_context(&db_lock, parsed_command_line)
        else {
            return Vec::new();
        };

        if let ArgumentType::Dynamic { provider, scope } = arg_type {
            drop(db_lock);
            if provider == "shell.job" {
                return self.collect_shell_job_candidates(&parsed_command_line.current_token);
            }
            return self.dynamic.collect_declared_dynamic_candidates(
                &provider,
                scope.as_deref(),
                parsed_command_line,
                current_dir,
                CachePolicy::CachedOnly,
            );
        }

        if !is_ghost_safe_argument_type(&arg_type) {
            return Vec::new();
        }

        let generator = ArgumentGenerator::new(&db_lock);
        generator
            .generate_candidates_for_type(&arg_type, parsed_command_line)
            .map(|candidates| {
                candidates
                    .into_iter()
                    .map(|candidate| self.convert_to_enhanced_candidate(candidate))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn collect_shell_job_candidates(&self, current_token: &str) -> Vec<EnhancedCandidate> {
        let jobs = self.shell_jobs.read();
        let mut candidates = Vec::with_capacity(jobs.len() + 2);

        if let Some((_, command, state)) = jobs.last() {
            candidates.push(EnhancedCandidate {
                text: "%+".to_string(),
                description: Some(format!("current job: {command} ({state})")),
                candidate_type: CandidateType::Argument,
                priority: 160,
            });
        }
        if jobs.len() >= 2
            && let Some((_, command, state)) = jobs.get(jobs.len() - 2)
        {
            candidates.push(EnhancedCandidate {
                text: "%-".to_string(),
                description: Some(format!("previous job: {command} ({state})")),
                candidate_type: CandidateType::Argument,
                priority: 155,
            });
        }
        candidates.extend(
            jobs.iter()
                .map(|(job_id, command, state)| EnhancedCandidate {
                    text: format!("%{job_id}"),
                    description: Some(format!("{command} ({state})")),
                    candidate_type: CandidateType::Argument,
                    priority: 150,
                }),
        );
        candidates.retain(|candidate| matches_prefix(current_token, &candidate.text));
        candidates
    }

    fn collect_pm_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        use parser::CompletionContext;

        let current_token = parsed_command_line.current_token.as_str();
        match parsed_command_line.completion_context {
            CompletionContext::SubCommand => pm_subcommand_candidates(current_token),
            CompletionContext::Argument { arg_index, .. } => {
                let Some(subcommand) = parsed_command_line.subcommand_path.first() else {
                    return Vec::new();
                };
                match subcommand.as_str() {
                    "add" => match arg_index {
                        0 => self.collect_directory_candidates(current_token),
                        1 => self.collect_project_name_candidates_from_path(
                            parsed_command_line,
                            current_token,
                        ),
                        _ => Vec::new(),
                    },
                    "work" | "remove" | "rm" | "jump" => {
                        self.collect_project_candidates(current_token)
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn collect_pj_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        let current_token = parsed_command_line.current_token.as_str();
        self.collect_project_candidates(current_token)
    }

    fn collect_project_candidates(&self, current_token: &str) -> Vec<EnhancedCandidate> {
        let Ok(mut projects) = project::list_projects() else {
            return Vec::new();
        };

        projects.sort_by_key(|project| std::cmp::Reverse(project.last_accessed));
        projects
            .into_iter()
            .filter(|project| matches_prefix(current_token, &project.name))
            .map(|project| EnhancedCandidate {
                text: project.name,
                description: Some(project.path.display().to_string()),
                candidate_type: CandidateType::Argument,
                priority: 90,
            })
            .collect()
    }

    fn collect_project_name_candidates_from_path(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        let Some(path) = parsed_command_line.specified_arguments.first() else {
            return Vec::new();
        };

        let trimmed = path.trim_end_matches(&['/', '\\'][..]);
        let Some(name) = std::path::Path::new(trimmed)
            .file_name()
            .and_then(|s| s.to_str())
        else {
            return Vec::new();
        };

        if !matches_prefix(current_token, name) {
            return Vec::new();
        }

        vec![EnhancedCandidate {
            text: name.to_string(),
            description: Some("from path".to_string()),
            candidate_type: CandidateType::Argument,
            priority: 95,
        }]
    }

    fn collect_directory_candidates(&self, current_token: &str) -> Vec<EnhancedCandidate> {
        match FileSystemGenerator::generate_directory_candidates(current_token) {
            Ok(candidates) => candidates
                .into_iter()
                .map(|candidate| self.convert_to_enhanced_candidate(candidate))
                .collect(),
            Err(e) => {
                warn!("Failed to load directory completions: {}", e);
                Vec::new()
            }
        }
    }

    fn collect_mcp_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
    ) -> Vec<EnhancedCandidate> {
        use parser::CompletionContext;

        let current_token = parsed_command_line.current_token.as_str();
        match parsed_command_line.completion_context {
            CompletionContext::SubCommand => mcp_subcommand_candidates(current_token),
            CompletionContext::Argument { .. } => {
                let Some(subcommand) = parsed_command_line.subcommand_path.first() else {
                    return Vec::new();
                };
                match subcommand.as_str() {
                    "connect" | "c" | "disconnect" | "d" => {
                        let env = self.environment.read();
                        let mut seen = std::collections::HashSet::new();
                        env.mcp_servers()
                            .iter()
                            .filter_map(|server| {
                                if !matches_prefix(current_token, &server.label) {
                                    return None;
                                }
                                if !seen.insert(server.label.clone()) {
                                    return None;
                                }
                                Some(EnhancedCandidate {
                                    text: server.label.clone(),
                                    description: mcp_description(server),
                                    candidate_type: CandidateType::Argument,
                                    priority: 90,
                                })
                            })
                            .collect()
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn collect_package_run_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        if !leading_completion_words_match(parsed_command_line, &["run"]) {
            return Vec::new();
        }
        self.dynamic.collect_project_task_candidates(
            parsed_command_line,
            current_dir,
            JS_TASK_SOURCES,
        )
    }

    fn collect_yarn_script_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        let leading_words = leading_completion_words(parsed_command_line);
        if !(leading_words.is_empty() || leading_words.as_slice() == ["run"]) {
            return Vec::new();
        }
        self.dynamic.collect_project_task_candidates(
            parsed_command_line,
            current_dir,
            JS_TASK_SOURCES,
        )
    }

    fn collect_deno_task_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        if !leading_completion_words_match(parsed_command_line, &["task"]) {
            return Vec::new();
        }
        self.dynamic.collect_project_task_candidates(
            parsed_command_line,
            current_dir,
            DENO_TASK_SOURCES,
        )
    }

    fn collect_top_level_task_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
        sources: &[&str],
    ) -> Vec<EnhancedCandidate> {
        if !leading_completion_words(parsed_command_line).is_empty() {
            return Vec::new();
        }
        self.dynamic
            .collect_project_task_candidates(parsed_command_line, current_dir, sources)
    }

    fn collect_external_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &ParsedCommandLine,
    ) -> CandidateBatch {
        let candidates = self.dynamic.collect_external_candidates(
            request.current_dir,
            request.input,
            request.cursor_pos,
            parsed_command_line,
        );
        if candidates.is_empty() {
            return CandidateBatch::empty();
        }

        CandidateBatch::inclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
    }

    fn collect_fish_fallback_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &ParsedCommandLine,
    ) -> CandidateBatch {
        if self.scalar_option_value_context(parsed_command_line) {
            return CandidateBatch::empty();
        }

        let candidates = self.dynamic.collect_fish_fallback_candidates(
            request.current_dir,
            request.input,
            request.cursor_pos,
            parsed_command_line,
        );
        if candidates.is_empty() {
            return CandidateBatch::empty();
        }

        CandidateBatch::inclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
    }

    fn scalar_option_value_context(&self, parsed_command_line: &ParsedCommandLine) -> bool {
        if !matches!(
            parsed_command_line.completion_context,
            parser::CompletionContext::OptionValue { .. }
        ) {
            return false;
        }

        matches!(
            self.argument_type_for_completion_context(parsed_command_line),
            Some(
                ArgumentType::String
                    | ArgumentType::Number
                    | ArgumentType::Url
                    | ArgumentType::Regex
            )
        )
    }

    /// Convert CompletionCandidate to EnhancedCandidate
    fn convert_to_enhanced_candidate(&self, candidate: CompletionCandidate) -> EnhancedCandidate {
        EnhancedCandidate {
            text: candidate.text,
            description: candidate.description,
            candidate_type: match candidate.completion_type {
                super::command::CompletionType::SubCommand => CandidateType::SubCommand,
                super::command::CompletionType::ShortOption => CandidateType::ShortOption,
                super::command::CompletionType::LongOption => CandidateType::LongOption,
                super::command::CompletionType::Argument => CandidateType::Argument,
                super::command::CompletionType::File => CandidateType::File,
                super::command::CompletionType::Directory => CandidateType::Directory,
                super::command::CompletionType::Process => CandidateType::Process,
            },
            priority: candidate.priority,
        }
    }

    /// Convert EnhancedCandidate list to Candidate list for skim display
    pub fn to_candidates(&self, enhanced_candidates: Vec<EnhancedCandidate>) -> Vec<Candidate> {
        enhanced_candidates
            .into_iter()
            .map(|ec| ec.to_candidate())
            .collect()
    }

    fn store_in_cache(
        &self,
        key: &str,
        candidates: &[EnhancedCandidate],
        framework: CompletionFrameworkKind,
    ) {
        if key.is_empty() || candidates.is_empty() {
            return;
        }
        debug!("cache set for '{}'. len: {}", key, candidates.len());

        self.cache.set(key.to_string(), candidates.to_vec());
        self.framework_cache
            .write()
            .insert(key.to_string(), framework);
    }

    fn lookup_cached_framework(&self, key: &str) -> Option<CompletionFrameworkKind> {
        self.framework_cache.read().get(key).copied()
    }

    /// Deduplication and sorting
    fn deduplicate_and_sort(
        &self,
        mut candidates: Vec<EnhancedCandidate>,
        max_results: usize,
        history: Option<&Arc<parking_lot::Mutex<crate::history::History>>>,
        command_context: Option<&str>,
    ) -> Vec<EnhancedCandidate> {
        // Boost priority based on history
        if let Some(history_arc) = history
            && let Some(history) = history_arc.try_lock()
        {
            let boosts = history_boost_scores(&candidates, &history, command_context);
            for (candidate, score) in candidates.iter_mut().zip(boosts) {
                if score == 0 {
                    continue;
                }
                candidate.priority = candidate.priority.saturating_add(score);
            }
        }

        // Sorting before dedup keeps the best-ranked candidate for duplicate text.
        candidates.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| {
                    a.candidate_type
                        .sort_order()
                        .cmp(&b.candidate_type.sort_order())
                })
                .then_with(|| a.text.cmp(&b.text))
        });
        // Normalize away a trailing path separator before deduping: different
        // candidate sources (e.g. the JSON/FileSystemGenerator stage vs. the
        // fish-fallback stage) disagree on whether a directory's text ends in
        // `/`, so comparing raw text would let the same directory survive twice.
        // The candidate_type is kept as part of the key so this normalization
        // can't accidentally merge an unrelated candidate (e.g. a git branch or
        // history entry) that happens to share text with a trimmed directory.
        let mut seen = HashSet::with_capacity(candidates.len());
        candidates.retain(|candidate| {
            let key = candidate.text.trim_end_matches(['/', '\\']);
            seen.insert((candidate.candidate_type.clone(), key.to_string()))
        });

        candidates.truncate(max_results);
        candidates
    }
}

fn history_boost_scores(
    candidates: &[EnhancedCandidate],
    history: &crate::history::History,
    command_context: Option<&str>,
) -> Vec<u32> {
    let mut scores = vec![0; candidates.len()];
    let mut candidate_indexes_by_token: HashMap<&str, Vec<usize>> = HashMap::new();

    for (index, candidate) in candidates.iter().enumerate() {
        if matches!(
            candidate.candidate_type,
            CandidateType::File | CandidateType::Directory
        ) {
            continue;
        }

        candidate_indexes_by_token
            .entry(candidate.text.as_str())
            .or_default()
            .push(index);
    }

    if candidate_indexes_by_token.is_empty() {
        return scores;
    }

    let command_prefix = command_context.map(|command| format!("{command} "));
    let mut capped = vec![false; candidates.len()];
    let mut capped_count = 0;
    let target_count = candidate_indexes_by_token
        .values()
        .map(Vec::len)
        .sum::<usize>();

    for item in history.iter().rev().take(HISTORY_BOOST_SCAN_LIMIT) {
        let context_bonus = if command_context.is_some_and(|command| item.entry == command)
            || command_prefix
                .as_ref()
                .is_some_and(|prefix| item.entry.starts_with(prefix))
        {
            500
        } else {
            0
        };

        let mut tokens_seen = HashSet::new();
        for token in item.entry.split_whitespace() {
            if !tokens_seen.insert(token) {
                continue;
            }

            let Some(indexes) = candidate_indexes_by_token.get(token) else {
                continue;
            };

            for &index in indexes {
                if capped[index] {
                    continue;
                }

                scores[index] = scores[index].saturating_add(10 + context_bonus);
                if scores[index] > HISTORY_BOOST_SCORE_CAP {
                    capped[index] = true;
                    capped_count += 1;
                }
            }
        }

        if capped_count == target_count {
            break;
        }
    }

    scores
}

fn argument_type_for_completion_context(
    database: &CommandCompletionDatabase,
    parsed: &ParsedCommandLine,
) -> Option<ArgumentType> {
    match &parsed.completion_context {
        parser::CompletionContext::Argument {
            arg_type: Some(arg_type),
            ..
        } => Some(arg_type.clone()),
        parser::CompletionContext::Argument { arg_index, .. } => {
            let command_completion = database.get_command(&parsed.command)?;
            let arguments =
                arguments_for_subcommand_path(command_completion, &parsed.subcommand_path);
            resolve_argument_definition(arguments, *arg_index)
                .and_then(|argument| argument.arg_type.clone())
        }
        parser::CompletionContext::OptionValue {
            option_name,
            value_type: Some(value_type),
        } => {
            let command_completion = database.get_command(&parsed.command)?;
            let option_value_type = option_for_subcommand_path(
                command_completion,
                &parsed.subcommand_path,
                option_name,
            )
            .and_then(CommandOption::value_type)
            .cloned();
            option_value_type.or_else(|| Some(value_type.clone()))
        }
        parser::CompletionContext::OptionValue {
            option_name,
            value_type: None,
        } => {
            let command_completion = database.get_command(&parsed.command)?;
            option_for_subcommand_path(command_completion, &parsed.subcommand_path, option_name)
                .and_then(CommandOption::value_type)
                .cloned()
        }
        _ => None,
    }
}

fn arguments_for_subcommand_path<'a>(
    command_completion: &'a super::command::CommandCompletion,
    path: &[String],
) -> &'a [super::command::Argument] {
    let mut arguments = &command_completion.arguments;
    let mut subcommands = &command_completion.subcommands;

    for name in path {
        let Some(subcommand) = find_matching_subcommand(subcommands, name) else {
            break;
        };
        arguments = &subcommand.arguments;
        subcommands = &subcommand.subcommands;
    }

    arguments
}

fn option_for_subcommand_path<'a>(
    command_completion: &'a super::command::CommandCompletion,
    path: &[String],
    option_name: &str,
) -> Option<&'a CommandOption> {
    let mut options = command_completion.global_options.iter().collect::<Vec<_>>();
    let mut subcommands = &command_completion.subcommands;

    for name in path {
        let Some(subcommand) = find_matching_subcommand(subcommands, name) else {
            break;
        };
        options.extend(subcommand.options.iter());
        subcommands = &subcommand.subcommands;
    }

    options
        .into_iter()
        .find(|option| option.matches_name(option_name))
}

fn resolve_argument_definition(
    arguments: &[super::command::Argument],
    arg_index: usize,
) -> Option<&super::command::Argument> {
    arguments.get(arg_index).or_else(|| {
        arguments
            .last()
            .filter(|argument| argument.multiple && !arguments.is_empty())
    })
}

fn find_matching_subcommand<'a>(
    subcommands: &'a [SubCommand],
    name: &str,
) -> Option<&'a SubCommand> {
    subcommands.iter().find(|subcommand| {
        subcommand.name == name || subcommand.aliases.iter().any(|alias| alias == name)
    })
}

fn is_ghost_safe_argument_type(arg_type: &ArgumentType) -> bool {
    matches!(
        arg_type,
        ArgumentType::Choice(_)
            | ArgumentType::Environment
            | ArgumentType::Process
            | ArgumentType::User
            | ArgumentType::Group
            | ArgumentType::Signal
            | ArgumentType::Interface
            | ArgumentType::Dynamic { .. }
    )
}

fn completion_replacement_range(
    input: &str,
    cursor_pos: usize,
    parsed_command_line: &ParsedCommandLine,
) -> Option<CompletionReplacementRange> {
    let token_range = token_range_at_cursor(input, cursor_pos)?;
    let token = slice_chars(input, token_range.start, token_range.end);

    if let parser::CompletionContext::OptionValue { option_name, .. } =
        &parsed_command_line.completion_context
        && let Some(value_range) =
            option_value_range_from_token(&token, token_range.start, option_name)
    {
        return Some(value_range);
    }

    Some(token_range)
}

fn option_value_range_from_token(
    token: &str,
    token_start: usize,
    option_name: &str,
) -> Option<CompletionReplacementRange> {
    if option_name.starts_with("--") {
        let prefix = format!("{option_name}=");
        if token.starts_with(&prefix) {
            let start = token_start + prefix.chars().count();
            let end = token_start + token.chars().count();
            return Some(CompletionReplacementRange { start, end });
        }
    }

    if option_name.len() == 2 && !option_name.starts_with("--") && token.starts_with(option_name) {
        let value = &token[option_name.len()..];
        if !value.is_empty() && !value.starts_with('=') {
            let start = token_start + option_name.chars().count();
            let end = token_start + token.chars().count();
            return Some(CompletionReplacementRange { start, end });
        }
    }

    None
}

fn token_range_at_cursor(input: &str, cursor_pos: usize) -> Option<CompletionReplacementRange> {
    let token =
        shell_token::token_at_char_cursor(input, cursor_pos, SeparatorMode::CompletionRange)?;
    Some(CompletionReplacementRange {
        start: token.char_start,
        end: token.char_end,
    })
}

fn slice_chars(input: &str, start: usize, end: usize) -> String {
    input
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn replace_char_range(input: &str, start: usize, end: usize, replacement: &str) -> String {
    let mut result = String::with_capacity(input.len() + replacement.len());
    for (index, ch) in input.chars().enumerate() {
        if index == start {
            result.push_str(replacement);
        }
        if index < start || index >= end {
            result.push(ch);
        }
    }
    if start >= input.chars().count() {
        result.push_str(replacement);
    }
    result
}

/// User-home (`~user`) candidates, reusing the `/etc/passwd`-backed user
/// generator. `prefix` is the partial user name (the part after `~`); the
/// returned text keeps the leading `~` so it replaces the whole token.
fn tilde_user_candidates(prefix: &str) -> Vec<EnhancedCandidate> {
    let generator = super::generators::user::UserGenerator::new();
    match generator.generate_candidates(prefix) {
        Ok(users) => users
            .into_iter()
            .map(|user| EnhancedCandidate {
                text: format!("~{}", user.text),
                description: user.description,
                candidate_type: CandidateType::Generic,
                priority: 140,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

pub(super) fn matches_prefix(current_token: &str, value: &str) -> bool {
    current_token.is_empty()
        || value.starts_with(current_token)
        || super::fuzzy_match_score(value, current_token).is_some()
}

/// Whether the cursor sits past the wrapped command's own name token.
///
/// `sudo pac<TAB>` is completing the command name itself and must keep the
/// wrapper's own completion; `sudo pacman -R <TAB>` is completing *for* the
/// wrapped command and should be unwrapped.
fn cursor_follows_wrapped_command(parsed: &ParsedCommandLine, command_name: &str) -> bool {
    let Some(command_index) = parsed
        .raw_args
        .iter()
        .position(|token| token == command_name)
    else {
        return false;
    };

    let cursor_index = if parsed.current_token.is_empty() {
        parsed.raw_args.len()
    } else {
        parsed
            .raw_args
            .iter()
            .rposition(|token| token == &parsed.current_token)
            .unwrap_or(parsed.raw_args.len())
    };

    cursor_index > command_index
}

fn is_dynamic_completion_command(command: &str) -> bool {
    DYNAMIC_PROVIDER_SPECS
        .iter()
        .any(|provider| provider.command == command)
}

fn completion_cache_allowed(parsed_command_line: &ParsedCommandLine) -> bool {
    // A wrapper (`sudo pacman -R`) parses as the wrapper's own command line, so
    // checking only `command` would let dynamic results be cached under the
    // wrapper. Scanning the arguments is cheap and errs toward not caching.
    let dynamic_command = is_dynamic_completion_command(&parsed_command_line.command)
        || parsed_command_line
            .specified_arguments
            .iter()
            .any(|argument| is_dynamic_completion_command(argument));

    if !dynamic_command {
        return true;
    }

    matches!(
        parsed_command_line.completion_context,
        parser::CompletionContext::Command
            | parser::CompletionContext::SubCommand
            | parser::CompletionContext::ShortOption
            | parser::CompletionContext::LongOption
    )
}

fn dynamic_candidates_are_exclusive(parsed_command_line: &ParsedCommandLine) -> bool {
    if parsed_command_line.command != "git" {
        return false;
    }

    if !matches!(
        parsed_command_line.completion_context,
        parser::CompletionContext::Argument { .. } | parser::CompletionContext::SubCommand
    ) {
        return false;
    }

    let Some(primary_subcommand) = parsed_command_line.subcommand_path.first() else {
        return false;
    };

    matches!(
        primary_subcommand.as_str(),
        "checkout" | "switch" | "merge" | "rebase" | "branch"
    )
}

/// Whether the current `git` subcommand accepts working-tree paths as an
/// argument (in addition to any refs). `git checkout` and `git restore` are
/// dual-purpose (switch branch OR restore a file), so file/directory candidates
/// must be offered alongside branch candidates.
fn git_subcommand_accepts_paths(parsed_command_line: &ParsedCommandLine) -> bool {
    if parsed_command_line.command != "git" {
        return false;
    }

    if !matches!(
        parsed_command_line.completion_context,
        parser::CompletionContext::Argument { .. }
    ) {
        return false;
    }

    matches!(
        parsed_command_line
            .subcommand_path
            .first()
            .map(String::as_str),
        Some("checkout") | Some("restore")
    )
}

fn collect_task_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine
            .dynamic
            .collect_task_candidates(parsed, request.current_dir, cache_policy)
    }
}

fn collect_pm_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_pm_candidates(parsed)
    }
}

fn collect_pj_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_pj_candidates(parsed)
    }
}

/// `skill show|path|remove|archive|unarchive|pin|unpin <TAB>` (skill names)
/// and `skill diff|approve|reject <TAB>` (pending proposal ids).
///
/// Both are chosen by the model, not by the person typing, so without this
/// the only way to learn either is to run `skill list`/`skill pending` first.
fn collect_skill_dynamic_candidates(
    _engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    use parser::CompletionContext;

    // Reading two directories is not work to do on a cached-only pass.
    if cache_policy.is_cached_only() {
        return Vec::new();
    }
    if !matches!(
        parsed.completion_context,
        CompletionContext::Argument { .. }
    ) {
        return Vec::new();
    }
    let Some(subcommand) = parsed.subcommand_path.first() else {
        return Vec::new();
    };
    let current_token = parsed.current_token.as_str();

    if matches!(subcommand.as_str(), "diff" | "approve" | "reject") {
        return dsh_builtin::pending_proposal_ids()
            .into_iter()
            .filter(|(id, _)| matches_prefix(current_token, id))
            .map(|(id, summary)| EnhancedCandidate {
                text: id,
                description: Some(summary),
                candidate_type: CandidateType::Argument,
                priority: 90,
            })
            .collect();
    }

    if !matches!(
        subcommand.as_str(),
        "show" | "path" | "remove" | "rm" | "archive" | "unarchive" | "pin" | "unpin"
    ) {
        return Vec::new();
    }

    dsh_builtin::installed_skill_names(Some(request.current_dir))
        .into_iter()
        .filter(|(name, _)| matches_prefix(current_token, name))
        .map(|(name, summary)| EnhancedCandidate {
            text: name,
            description: Some(summary),
            candidate_type: CandidateType::Argument,
            priority: 90,
        })
        .collect()
}

fn collect_mcp_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_mcp_candidates(parsed)
    }
}

fn collect_git_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_git_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_docker_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_docker_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_kubectl_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_kubectl_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_cargo_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_cargo_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_systemctl_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_systemctl_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_journalctl_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_journalctl_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_ssh_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_ssh_host_candidates(parsed, request.current_dir, "ssh", cache_policy)
}

fn collect_scp_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_ssh_host_candidates(parsed, request.current_dir, "scp", cache_policy)
}

fn collect_rsync_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_ssh_host_candidates(parsed, request.current_dir, "rsync", cache_policy)
}

fn collect_tmux_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_tmux_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_screen_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_screen_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_pgrep_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_process_name_candidates(parsed, "pgrep", cache_policy)
}

fn collect_pkill_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_process_name_candidates(parsed, "pkill", cache_policy)
}

fn collect_pip_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_pip_candidates(parsed, request.current_dir, "pip", cache_policy)
}

fn collect_pip3_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_pip_candidates(parsed, request.current_dir, "pip3", cache_policy)
}

fn collect_rustup_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_rustup_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_gh_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_gh_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_nmcli_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_nmcli_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_pacman_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_pacman_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_mount_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_mount_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_umount_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_umount_candidates(parsed, request.current_dir, cache_policy)
}

fn collect_modprobe_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_modprobe_candidates(parsed, cache_policy)
}

fn collect_tcpdump_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_tcpdump_candidates(parsed, cache_policy)
}

fn collect_npm_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    let mut candidates = if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_package_run_candidates(parsed, request.current_dir)
    };
    let completes_dependency = match parsed.command.as_str() {
        "pnpm" => {
            leading_completion_words_match(parsed, &["remove"])
                || leading_completion_words_match(parsed, &["update"])
                || leading_completion_words_match(parsed, &["why"])
        }
        _ => {
            leading_completion_words_match(parsed, &["uninstall"])
                || leading_completion_words_match(parsed, &["update"])
        }
    };
    if completes_dependency {
        candidates.extend(engine.dynamic.collect_js_dependency_candidates(
            parsed,
            request.current_dir,
            parsed.command.as_str(),
            cache_policy.is_cached_only(),
        ));
    }
    candidates
}

fn collect_yarn_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    let mut candidates = if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_yarn_script_candidates(parsed, request.current_dir)
    };
    if leading_completion_words_match(parsed, &["remove"])
        || leading_completion_words_match(parsed, &["why"])
        || leading_completion_words_match(parsed, &["upgrade"])
    {
        candidates.extend(engine.dynamic.collect_js_dependency_candidates(
            parsed,
            request.current_dir,
            "yarn",
            cache_policy.is_cached_only(),
        ));
    }
    candidates
}

fn collect_deno_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_deno_task_candidates(parsed, request.current_dir)
    }
}

fn collect_just_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_top_level_task_candidates(parsed, request.current_dir, JUST_TASK_SOURCES)
    }
}

fn collect_make_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
    cache_policy: CachePolicy,
) -> Vec<EnhancedCandidate> {
    if cache_policy.is_cached_only() {
        Vec::new()
    } else {
        engine.collect_top_level_task_candidates(parsed, request.current_dir, MAKE_TASK_SOURCES)
    }
}

fn leading_completion_words(parsed: &ParsedCommandLine) -> Vec<&str> {
    let mut words: Vec<&str> = if parsed.subcommand_path.is_empty() {
        parsed
            .specified_arguments
            .iter()
            .map(String::as_str)
            .collect()
    } else {
        parsed.subcommand_path.iter().map(String::as_str).collect()
    };

    if words.last().copied() == Some(parsed.current_token.as_str()) {
        words.pop();
    }

    words
}

fn leading_completion_words_match(parsed: &ParsedCommandLine, expected: &[&str]) -> bool {
    leading_completion_words(parsed).as_slice() == expected
}

fn pm_subcommand_candidates(current_token: &str) -> Vec<EnhancedCandidate> {
    let items = [
        ("init", "Register the current project root"),
        ("status", "Show current project status"),
        ("st", "Alias for status"),
        ("add", "Register a project"),
        ("list", "List registered projects"),
        ("ls", "Alias for list"),
        ("remove", "Remove a project"),
        ("rm", "Alias for remove"),
        ("work", "Switch to a project"),
        ("jump", "Select a project interactively"),
        ("activate", "Activate current project environment"),
    ];

    items
        .iter()
        .filter(|(name, _)| matches_prefix(current_token, name))
        .map(|(name, desc)| EnhancedCandidate {
            text: (*name).to_string(),
            description: Some((*desc).to_string()),
            candidate_type: CandidateType::SubCommand,
            priority: 110,
        })
        .collect()
}

fn mcp_subcommand_candidates(current_token: &str) -> Vec<EnhancedCandidate> {
    let items = [
        ("status", "Show connection status"),
        ("s", "Alias for status"),
        ("connect", "Connect to a MCP server"),
        ("c", "Alias for connect"),
        ("disconnect", "Disconnect a MCP server"),
        ("d", "Alias for disconnect"),
        ("list", "List registered MCP servers"),
        ("l", "Alias for list"),
        ("tools", "List MCP tools"),
        ("t", "Alias for tools"),
        ("help", "Show help"),
    ];

    items
        .iter()
        .filter(|(name, _)| matches_prefix(current_token, name))
        .map(|(name, desc)| EnhancedCandidate {
            text: (*name).to_string(),
            description: Some((*desc).to_string()),
            candidate_type: CandidateType::SubCommand,
            priority: 110,
        })
        .collect()
}

fn mcp_description(server: &dsh_types::mcp::McpServerConfig) -> Option<String> {
    if let Some(description) = &server.description
        && !description.trim().is_empty()
    {
        return Some(description.clone());
    }

    match &server.transport {
        McpTransport::Stdio { command, .. } => Some(format!("stdio: {}", command)),
        McpTransport::Sse { url } => Some(format!("sse: {}", url)),
        McpTransport::Http { url, .. } => Some(format!("http: {}", url)),
    }
}

/// Enhanced completion candidate
#[derive(Debug, Clone)]
pub struct EnhancedCandidate {
    /// Candidate text
    pub text: String,
    /// Description
    pub description: Option<String>,
    /// Candidate type
    pub candidate_type: CandidateType,
    /// Priority
    pub priority: u32,
}

impl EnhancedCandidate {
    /// Convert to Candidate for skim display
    pub fn to_candidate(&self) -> Candidate {
        match self.candidate_type {
            CandidateType::SubCommand => Candidate::Command {
                name: self.text.clone(),
                description: self.description.clone().unwrap_or_default(),
            },
            CandidateType::LongOption | CandidateType::ShortOption => Candidate::Option {
                name: self.text.clone(),
                description: self.description.clone().unwrap_or_default(),
            },
            CandidateType::File => Candidate::File {
                path: self.text.clone(),
                is_dir: false,
            },
            CandidateType::Directory => Candidate::File {
                path: self.text.clone(),
                is_dir: true,
            },
            CandidateType::Argument | CandidateType::Generic => {
                if let Some(ref desc) = self.description {
                    Candidate::Item(self.text.clone(), desc.clone())
                } else {
                    Candidate::Basic(self.text.clone())
                }
            }
            CandidateType::Process => Candidate::Process {
                pid: self.text.clone(),
                command: self.description.clone().unwrap_or_default(),
            },
        }
    }
}

/// Candidate type
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CandidateType {
    /// Subcommand
    SubCommand,
    /// Short option
    ShortOption,
    /// Long option
    LongOption,
    /// Argument
    Argument,
    /// File
    File,
    /// Directory
    Directory,
    /// Process
    Process,
    /// Generic
    Generic,
}

impl CandidateType {
    /// Get sort order
    pub fn sort_order(&self) -> u8 {
        match self {
            CandidateType::SubCommand => 1,
            CandidateType::LongOption => 2,
            CandidateType::ShortOption => 3,
            CandidateType::Argument => 4,
            CandidateType::Directory => 5,
            CandidateType::File => 6,
            CandidateType::Process => 7,
            CandidateType::Generic => 8,
        }
    }

    /// Get display icon
    pub fn icon(&self) -> &'static str {
        match self {
            CandidateType::SubCommand => "⚡",
            CandidateType::LongOption => "🔧",
            CandidateType::ShortOption => "🔧",
            CandidateType::Argument => "📝",
            CandidateType::File => "📄",
            CandidateType::Directory => "📁",
            CandidateType::Process => "🔧",
            CandidateType::Generic => "💡",
        }
    }
}

#[cfg(test)]
mod tests;
