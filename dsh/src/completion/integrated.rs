//! `IntegratedCompletionEngine`: the orchestrator that turns a parsed command
//! line into ranked `EnhancedCandidate`s, merging the JSON-declared command
//! database, the command-name-keyed dynamic providers (`providers.rs`), ghost
//! text, history-frequency boosts (`scoring.rs`), and shell state
//! (variables, jobs, aliases). The engine's own struct, cache, and `complete`
//! entry point stay in this file; supporting types and free functions live
//! in the sibling modules below.
use super::cache::CompletionCache;
use super::command::{
    ArgumentType, CommandCompletionDatabase, CommandOption, CompletionCandidate, SubCommand,
};
use super::context::ContextCorrector;
use super::dynamic::{CachePolicy, CompletionRuntime, DynamicCompletionProvider};

use super::framework::CompletionFrameworkKind;

use super::generator::CompletionGenerator;
use crate::completion::generators::filesystem::FileSystemGenerator;

use super::json_loader::JsonCompletionLoader;
use super::parser::{self, CommandLineParser, ParsedCommandLine};
use crate::completion::display::Candidate;
use crate::completion::generators::argument::ArgumentGenerator;
use crate::environment::Environment;
use anyhow::Result;
use dsh_builtin::project;
use parking_lot::{Mutex, RwLock};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

mod candidate;
mod command_candidates;
mod dynamic_dispatch;
mod finalize;
mod ghost;
mod predicates;
mod providers;
mod range;
mod scoring;
mod sources;
mod timing;

pub use candidate::{CandidateType, EnhancedCandidate};
pub(crate) use predicates::matches_prefix;
use predicates::*;
use providers::*;
pub use range::CompletionReplacementRange;
use range::*;
use scoring::*;
use timing::*;

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

#[derive(Debug, Clone, Copy)]
struct CompletionRequest<'a> {
    input: &'a str,
    current_dir: &'a Path,
    max_results: usize,
    cursor_pos: usize,
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
        // A request that starts while dynamic data is in flight stays
        // non-cacheable for its lifetime: the refresh may finish after this
        // request has already observed only part of the old/new state, so a
        // later "pending became false" is not proof that this request built a
        // stable snapshot. A later request may publish the settled result.
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
        // The pre-collection `cache_allowed` is only ever narrowed here, never
        // widened: a refresh scheduled mid-collection means this result may be
        // an old/new mix, so it must not be published even if nothing is
        // pending anymore by the time collection finishes.
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
}

#[cfg(test)]
mod tests;
