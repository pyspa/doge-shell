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
mod predicates;
mod providers;
mod range;
mod scoring;
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

#[cfg(test)]
mod tests;
