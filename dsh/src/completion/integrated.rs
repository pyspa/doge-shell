use super::cache::CompletionCache;
use super::command::{CommandCompletionDatabase, CompletionCandidate};
use super::dynamic::DynamicCompletionProvider;

use super::framework::CompletionFrameworkKind;

use super::generator::CompletionGenerator;
use super::shell_token::{self, SeparatorMode};
use crate::completion::generators::filesystem::FileSystemGenerator;

use super::json_loader::JsonCompletionLoader;
use super::parser::{self, CommandLineParser, ParsedCommandLine};
use crate::completion::display::Candidate;
use crate::environment::Environment;
use anyhow::Result;
use dsh_builtin::project;
use dsh_types::mcp::McpTransport;
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

const DEFAULT_CACHE_TTL_MS: u64 = 3000;
const HISTORY_BOOST_SCAN_LIMIT: usize = 2000;
const HISTORY_BOOST_SCORE_CAP: u32 = 5000;

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
            return true;
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
    /// Command line parser
    parser: CommandLineParser,

    /// Dynamic completion registry

    /// Short lived completion cache
    cache: CompletionCache<EnhancedCandidate>,
    framework_cache: RwLock<HashMap<String, CompletionFrameworkKind>>,
    dynamic: DynamicCompletionProvider,

    /// Shell environment (for dynamic completion)
    pub environment: Arc<RwLock<Environment>>,
}

impl IntegratedCompletionEngine {
    /// Create a new integrated completion engine
    pub fn new(environment: Arc<RwLock<Environment>>) -> Self {
        Self {
            command_completion: Arc::new(Mutex::new(CommandCompletionDatabase::new())),
            loader: None,
            parser: CommandLineParser::new(),

            cache: CompletionCache::new(Duration::from_millis(DEFAULT_CACHE_TTL_MS)),
            framework_cache: RwLock::new(HashMap::new()),
            dynamic: DynamicCompletionProvider::new(environment.clone()),
            environment,
        }
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
        let mut parsed = self.parser.parse(input, cursor_pos);

        // For dynamic completion, update command with resolved alias
        parsed.command = self.environment.read().resolve_alias(&parsed.command);

        self.ensure_command_completion_loaded(&parsed.command);
        {
            let db_lock = self.command_completion.lock();
            if db_lock.get_command(&parsed.command).is_some() {
                parsed = CompletionGenerator::new(&db_lock).correct_parsed_command_line(&parsed);
            }
        }

        // Update args to use specified_arguments and options to use specified_options
        parsed.args = parsed.specified_arguments.clone();
        parsed.options = parsed.specified_options.clone();

        parsed
    }

    fn ensure_command_completion_loaded(&self, command_name: &str) {
        if command_name.is_empty() {
            return;
        }

        let Some(loader) = &self.loader else {
            return;
        };

        let mut db = self.command_completion.lock();
        if db.get_command(command_name).is_some() {
            return;
        }

        debug!("Lazy loading completion for command: {}", command_name);
        match loader.load_command_completion(command_name) {
            Ok(Some(completion)) => {
                db.add_command(completion);
            }
            Ok(None) => {
                debug!("No completion definition found for {}", command_name);
            }
            Err(e) => {
                warn!("Failed to load completion for {}: {}", command_name, e);
            }
        }
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

        let parsed_command_line = self.convert_to_parsed_command_line(input, cursor_pos);
        let replacement_range =
            completion_replacement_range(input, cursor_pos, &parsed_command_line);

        if !request.input.is_empty()
            && let Some(hit) = self.cache.lookup(request.input)
        {
            debug!(
                "cache hit for '{}' (key: '{}', exact: {})",
                request.input, hit.key, hit.exact
            );

            if hit.exact || !hit.candidates.is_empty() {
                self.cache.extend_ttl(&hit.key);
                let framework = self
                    .lookup_cached_framework(&hit.key)
                    .unwrap_or_else(super::default_completion_framework);

                return CompletionResult {
                    candidates: hit.candidates,
                    framework,
                    replacement_range,
                };
            }
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
        if !aggregator.extend(dynamic_batch) {
            let mut results = aggregator.finalize(history);
            results.replacement_range = replacement_range;
            self.store_in_cache(request.input, &results.candidates, results.framework);
            return results;
        }

        // 2. JSON-based command completion
        let command_collection = self.collect_command_candidates(&request, &parsed_command_line);
        if !aggregator.extend(command_collection.batch) {
            let mut results = aggregator.finalize(history);
            results.replacement_range = replacement_range;
            self.store_in_cache(request.input, &results.candidates, results.framework);
            return results;
        }

        // 3. External completer fallback
        let external_batch = self.collect_external_candidates(&request, &parsed_command_line);
        if !aggregator.extend(external_batch) {
            let mut results = aggregator.finalize(history);
            results.replacement_range = replacement_range;
            self.store_in_cache(request.input, &results.candidates, results.framework);
            return results;
        }

        let mut results = aggregator.finalize(history);
        results.replacement_range = replacement_range;
        self.store_in_cache(request.input, &results.candidates, results.framework);
        results
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

    fn collect_dynamic_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
    ) -> CandidateBatch {
        let candidates = match parsed_command_line.command.as_str() {
            "task" => self
                .dynamic
                .collect_task_candidates(parsed_command_line, request.current_dir),
            "pm" => self.collect_pm_candidates(parsed_command_line),
            "pj" => self.collect_pj_candidates(parsed_command_line),
            "mcp" => self.collect_mcp_candidates(parsed_command_line),
            "git" => self
                .dynamic
                .collect_git_candidates(parsed_command_line, request.current_dir),
            "docker" => self
                .dynamic
                .collect_docker_candidates(parsed_command_line, request.current_dir),
            "kubectl" => self
                .dynamic
                .collect_kubectl_candidates(parsed_command_line, request.current_dir),
            _ => Vec::new(),
        };

        if candidates.is_empty() {
            CandidateBatch::empty()
        } else {
            CandidateBatch::inclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
        }
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
        if key.is_empty() {
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

        // Text-based deduplication (keep higher priority ones)
        candidates.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.text.cmp(&b.text))
        });

        let mut seen = std::collections::HashSet::new();
        candidates.retain(|candidate| {
            if seen.contains(&candidate.text) {
                false
            } else {
                seen.insert(candidate.text.clone());
                true
            }
        });

        // Final sorting (priority -> type -> alphabetical order)
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

pub(super) fn matches_prefix(current_token: &str, value: &str) -> bool {
    current_token.is_empty() || super::fuzzy_match_score(value, current_token).is_some()
}

fn pm_subcommand_candidates(current_token: &str) -> Vec<EnhancedCandidate> {
    let items = [
        ("add", "Register a project"),
        ("list", "List registered projects"),
        ("ls", "Alias for list"),
        ("remove", "Remove a project"),
        ("rm", "Alias for remove"),
        ("work", "Switch to a project"),
        ("jump", "Select a project interactively"),
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
#[derive(Debug, Clone, PartialEq)]
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn test_integrated_completion_engine_creation() {
        let engine = IntegratedCompletionEngine::new(Environment::new());
        assert!(engine.loader.is_none());
    }

    #[test]
    fn test_candidate_type_sorting() {
        let mut types = [
            CandidateType::File,
            CandidateType::SubCommand,
            CandidateType::LongOption,
            CandidateType::Directory,
        ];

        types.sort_by_key(|t| t.sort_order());

        assert_eq!(types[0], CandidateType::SubCommand);
        assert_eq!(types[1], CandidateType::LongOption);
        assert_eq!(types[2], CandidateType::Directory);
        assert_eq!(types[3], CandidateType::File);
    }

    #[test]
    fn test_enhanced_candidate_creation() {
        let candidate = EnhancedCandidate {
            text: "test".to_string(),
            description: Some("Test command".to_string()),
            candidate_type: CandidateType::SubCommand,
            priority: 100,
        };

        assert_eq!(candidate.text, "test");
        assert_eq!(candidate.candidate_type.icon(), "⚡");
        assert_eq!(candidate.candidate_type.sort_order(), 1);
    }

    #[test]
    fn test_enhanced_candidate_to_candidate_conversion() {
        let enhanced_candidate = EnhancedCandidate {
            text: "commit".to_string(),
            description: Some("Record changes to the repository".to_string()),
            candidate_type: CandidateType::SubCommand,
            priority: 100,
        };

        let candidate = enhanced_candidate.to_candidate();

        match candidate {
            Candidate::Command { name, description } => {
                assert_eq!(name, "commit");
                assert_eq!(description, "Record changes to the repository");
            }
            _ => panic!("Expected Command candidate"),
        }
    }

    #[test]
    fn token_range_at_cursor_preserves_double_quoted_spaces() {
        let input = r#"cat "dir with space/foo"#;
        let cursor_before_last_o = r#"cat "dir with space/fo"#.chars().count();

        assert_eq!(
            token_range_at_cursor(input, cursor_before_last_o),
            Some(CompletionReplacementRange { start: 4, end: 23 })
        );
        assert_eq!(
            slice_chars(input, 4, 23),
            r#""dir with space/foo"#.to_string()
        );
    }

    #[test]
    fn token_range_at_cursor_preserves_backslash_escaped_spaces() {
        let input = r#"cat dir\ with\ space/foo"#;
        let cursor_before_last_o = r#"cat dir\ with\ space/fo"#.chars().count();

        assert_eq!(
            token_range_at_cursor(input, cursor_before_last_o),
            Some(CompletionReplacementRange { start: 4, end: 24 })
        );
        assert_eq!(
            slice_chars(input, 4, 24),
            r#"dir\ with\ space/foo"#.to_string()
        );
    }

    #[test]
    fn shell_token_range_supplies_raw_token_for_path_formatting() {
        let input = r#"cat "dir with space/fo"#;
        let range = token_range_at_cursor(input, input.chars().count()).unwrap();
        let raw_token = slice_chars(input, range.start, range.end);
        let candidates = vec![Candidate::File {
            path: "dir with space/foo".to_string(),
            is_dir: false,
        }];

        let formatted = crate::completion::shell_path::format_candidates_for_token(
            candidates,
            Some(&raw_token),
        );

        assert_eq!(
            formatted[0],
            Candidate::File {
                path: r#""dir with space/foo"#.to_string(),
                is_dir: false,
            }
        );
    }

    #[tokio::test]
    async fn quoted_path_completion_generates_and_formats_candidate() {
        let dir = tempdir().unwrap();
        let spaced_dir = dir.path().join("dir with space");
        fs::create_dir(&spaced_dir).unwrap();
        fs::write(spaced_dir.join("foo"), "").unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = format!(r#"cat "{}/fo"#, spaced_dir.display());
        let result = engine
            .complete(&input, input.chars().count(), dir.path(), 50, None)
            .await;
        let expected = spaced_dir.join("foo").to_string_lossy().to_string();

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == expected),
            "expected normalized file candidate {:?} in {:?}",
            expected,
            result.candidates
        );

        let range = result.replacement_range.expect("replacement range");
        let raw_token = slice_chars(&input, range.start, range.end);
        let formatted = crate::completion::shell_path::format_candidates_for_token(
            engine.to_candidates(result.candidates),
            Some(&raw_token),
        );

        assert!(
            formatted.iter().any(|candidate| {
                matches!(
                    candidate,
                    Candidate::File { path, is_dir: false }
                        if path == &format!("\"{expected}")
                )
            }),
            "expected quoted display candidate in {:?}",
            formatted
        );
    }

    #[tokio::test]
    async fn escaped_path_completion_generates_and_formats_candidate() {
        let dir = tempdir().unwrap();
        let spaced_dir = dir.path().join("dir with space");
        fs::create_dir(&spaced_dir).unwrap();
        fs::write(spaced_dir.join("foo"), "").unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let escaped_prefix = spaced_dir
            .join("fo")
            .to_string_lossy()
            .replace(' ', r#"\ "#);
        let input = format!("cat {escaped_prefix}");
        let result = engine
            .complete(&input, input.chars().count(), dir.path(), 50, None)
            .await;
        let expected = spaced_dir.join("foo").to_string_lossy().to_string();

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == expected),
            "expected normalized file candidate {:?} in {:?}",
            expected,
            result.candidates
        );

        let range = result.replacement_range.expect("replacement range");
        let raw_token = slice_chars(&input, range.start, range.end);
        let formatted = crate::completion::shell_path::format_candidates_for_token(
            engine.to_candidates(result.candidates),
            Some(&raw_token),
        );
        let escaped_expected = expected.replace(' ', r#"\ "#);

        assert!(
            formatted.iter().any(|candidate| {
                matches!(
                    candidate,
                    Candidate::File { path, is_dir: false } if path == &escaped_expected
                )
            }),
            "expected escaped display candidate in {:?}",
            formatted
        );
    }

    #[tokio::test]
    async fn json_completion_filters_by_current_token() {
        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine
            .initialize_command_completion()
            .expect("command completion should initialize");

        let input = "git a";
        let cursor_pos = input.len();
        let current_dir = std::env::current_dir().expect("current dir available");

        let completion_result = engine
            .complete(input, cursor_pos, &current_dir, 50, None)
            .await;

        assert!(
            !completion_result.candidates.is_empty(),
            "expected git subcommand suggestions"
        );

        for candidate in &completion_result.candidates {
            assert!(
                candidate.text.starts_with('a'),
                "candidate '{}' should be filtered by prefix",
                candidate.text
            );
        }

        assert!(
            completion_result.candidates.iter().any(|c| c.text == "add"),
            "git add should remain available"
        );

        assert_eq!(
            completion_result.framework,
            crate::completion::framework::CompletionFrameworkKind::Skim
        );
    }

    #[test]
    fn history_boost_skips_file_candidates() {
        let environment = Environment::new();
        let engine = IntegratedCompletionEngine::new(environment);

        let mut history = crate::history::History::new();
        history.add_test_entry("bar.txt");
        history.add_test_entry("bar");
        let history_arc = std::sync::Arc::new(parking_lot::Mutex::new(history));

        let candidates = vec![
            EnhancedCandidate {
                text: "bar.txt".to_string(),
                description: None,
                candidate_type: CandidateType::File,
                priority: 0,
            },
            EnhancedCandidate {
                text: "bar".to_string(),
                description: None,
                candidate_type: CandidateType::Argument,
                priority: 0,
            },
        ];

        let boosted = engine.deduplicate_and_sort(candidates, 10, Some(&history_arc), None);
        let file_candidate = boosted.iter().find(|c| c.text == "bar.txt").unwrap();
        let arg_candidate = boosted.iter().find(|c| c.text == "bar").unwrap();

        assert_eq!(file_candidate.priority, 0);
        assert!(arg_candidate.priority > 0);
    }

    #[test]
    fn history_boost_does_not_leak_to_same_text_file_candidates() {
        let mut history = crate::history::History::new();
        history.add_test_entry("git checkout shared");

        let candidates = vec![
            EnhancedCandidate {
                text: "shared".to_string(),
                description: None,
                candidate_type: CandidateType::File,
                priority: 0,
            },
            EnhancedCandidate {
                text: "shared".to_string(),
                description: None,
                candidate_type: CandidateType::Argument,
                priority: 0,
            },
        ];

        let scores = history_boost_scores(&candidates, &history, Some("git"));

        assert_eq!(scores[0], 0);
        assert!(scores[1] >= 500);
    }

    #[test]
    fn test_context_aware_frecency_boost() {
        let environment = Environment::new();
        let engine = IntegratedCompletionEngine::new(environment);

        let mut history = crate::history::History::new();
        // Add history items: "git checkout" is frequent, "docker checkout" also exists
        history.add_test_entry("git checkout");
        history.add_test_entry("docker checkout");

        let history_arc = std::sync::Arc::new(parking_lot::Mutex::new(history));

        let create_candidate = || EnhancedCandidate {
            text: "checkout".to_string(),
            description: None,
            candidate_type: CandidateType::SubCommand,
            priority: 0,
        };

        // Case 1: Context is "git"
        let candidates_git = vec![create_candidate()];
        let boosted_git =
            engine.deduplicate_and_sort(candidates_git, 10, Some(&history_arc), Some("git"));
        let score_git = boosted_git[0].priority;

        // Case 2: Context is "npm" (irrelevant)
        let candidates_npm = vec![create_candidate()];
        let boosted_npm =
            engine.deduplicate_and_sort(candidates_npm, 10, Some(&history_arc), Some("npm"));
        let score_npm = boosted_npm[0].priority;

        // "git" context should boost "git checkout" history item highly
        // "npm" context should only get base boost
        assert!(
            score_git > score_npm,
            "Context match should produce higher priority. git: {}, npm: {}",
            score_git,
            score_npm
        );

        // Ensure the boost is substantial (our logic adds 500)
        assert!(score_git >= 500);
    }

    #[test]
    fn test_context_aware_frecency_boost_edge_cases() {
        let environment = Environment::new();
        let engine = IntegratedCompletionEngine::new(environment);

        // Setup history
        // Setup history
        let mut history = crate::history::History::new();
        history.add_test_entry("git checkout");
        let history_arc = std::sync::Arc::new(parking_lot::Mutex::new(history));

        let create_candidate = || EnhancedCandidate {
            text: "checkout".to_string(), // Matches "git checkout"
            description: None,
            candidate_type: CandidateType::SubCommand,
            priority: 0,
        };

        // Case 1: No context (None)
        // Should only get base boost from matching text "checkout" inside "git checkout"
        let candidates_none = vec![create_candidate()];
        let boosted_none =
            engine.deduplicate_and_sort(candidates_none, 10, Some(&history_arc), None);
        let score_none = boosted_none[0].priority;

        // Case 2: Matching context ("git")
        // Should get high boost
        let candidates_git = vec![create_candidate()];
        let boosted_git =
            engine.deduplicate_and_sort(candidates_git, 10, Some(&history_arc), Some("git"));
        let score_git = boosted_git[0].priority;

        assert!(
            score_git > score_none,
            "Git context should boost higher than no context"
        );
        assert!(
            score_none > 0,
            "Even without context, text match should give some boost"
        );

        // Case 3: Mismatching context ("docker")
        // Should behave same as None/Low boost, or strictly less if logic changes?
        // Logic: if context is some, and doesn't match start, it skips the BIG boost.
        // But the item "git checkout" does NOT start with "docker".
        // So no BIG boost.
        // Base boost (+10) still applies because item contains "checkout".
        let candidates_docker = vec![create_candidate()];
        let boosted_docker =
            engine.deduplicate_and_sort(candidates_docker, 10, Some(&history_arc), Some("docker"));
        let score_docker = boosted_docker[0].priority;

        assert_eq!(
            score_docker, score_none,
            "Mismatch context should have same score as no context (base match)"
        );
    }

    #[tokio::test]
    async fn git_checkout_completes_local_branches() {
        let dir = tempdir().unwrap();

        std::process::Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        fs::write(dir.path().join("README.md"), "hello\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["checkout", "-b", "feature/test-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "git checkout feat";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "feature/test-branch"),
            "expected dynamic git branch completion"
        );
    }

    #[tokio::test]
    async fn kubectl_context_option_value_uses_dynamic_provider() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let kubectl = bin_dir.join("kubectl");
        fs::write(
            &kubectl,
            "#!/bin/sh\nif [ \"$1\" = \"config\" ] && [ \"$2\" = \"get-contexts\" ]; then\n  printf 'dev-cluster\\nprod-cluster\\n'\nfi\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&kubectl).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&kubectl, permissions).unwrap();

        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![bin_dir.display().to_string()];
            env.clear_command_cache();
        }
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "kubectl --context de";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "dev-cluster"),
            "expected kubectl context completion"
        );
        assert_eq!(
            result.replacement_range,
            Some(CompletionReplacementRange { start: 18, end: 20 })
        );
    }

    #[tokio::test]
    async fn kubectl_inline_context_option_value_uses_dynamic_provider() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let kubectl = bin_dir.join("kubectl");
        fs::write(
            &kubectl,
            "#!/bin/sh\nif [ \"$1\" = \"config\" ] && [ \"$2\" = \"get-contexts\" ]; then\n  printf 'dev-cluster\\nprod-cluster\\n'\nfi\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&kubectl).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&kubectl, permissions).unwrap();

        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![bin_dir.display().to_string()];
            env.clear_command_cache();
        }
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "kubectl --context=de";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "dev-cluster"),
            "expected clean kubectl context completion"
        );
        assert_eq!(
            result.replacement_range,
            Some(CompletionReplacementRange { start: 18, end: 20 })
        );

        let cached = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            cached
                .candidates
                .iter()
                .any(|candidate| candidate.text == "dev-cluster"),
            "expected cached kubectl context completion"
        );
        assert_eq!(
            cached.replacement_range,
            Some(CompletionReplacementRange { start: 18, end: 20 })
        );

        let cursor_inside_value = "kubectl --context=d".len();
        let middle = engine
            .complete(input, cursor_inside_value, dir.path(), 50, None)
            .await;
        assert_eq!(
            middle.replacement_range,
            Some(CompletionReplacementRange { start: 18, end: 20 })
        );
    }

    #[tokio::test]
    async fn kubectl_short_attached_namespace_option_value_uses_dynamic_provider() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let kubectl = bin_dir.join("kubectl");
        fs::write(
            &kubectl,
            "#!/bin/sh\nif [ \"$1\" = \"get\" ] && [ \"$2\" = \"namespaces\" ]; then\n  printf 'dev-namespace\\nprod-namespace\\n'\nfi\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&kubectl).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&kubectl, permissions).unwrap();

        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![bin_dir.display().to_string()];
            env.clear_command_cache();
        }
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "kubectl -nde";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "dev-namespace"),
            "expected short attached kubectl namespace completion"
        );
        assert_eq!(
            result.replacement_range,
            Some(CompletionReplacementRange { start: 10, end: 12 })
        );
    }

    #[tokio::test]
    async fn replacement_range_uses_entire_token_under_cursor() {
        let dir = tempdir().unwrap();
        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "pm li";
        let cursor_after_l = "pm l".len();
        let result = engine
            .complete(input, cursor_after_l, dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "list"),
            "expected pm subcommand completion"
        );
        assert_eq!(
            result.replacement_range,
            Some(CompletionReplacementRange { start: 3, end: 5 })
        );
    }

    #[tokio::test]
    async fn external_completer_runs_as_fallback() {
        let dir = tempdir().unwrap();
        let environment = Environment::new();
        environment.write().variables.insert(
            "DSH_EXTERNAL_COMPLETER".to_string(),
            "printf 'zzint-alpha\\tExternal completer\\n'; printf 'unrelated-candidate\\tExternal completer\\n'"
                .to_string(),
        );

        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "unknown-command zzint";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "zzint-alpha"),
            "expected external completer fallback"
        );
        assert_eq!(
            result.replacement_range,
            Some(CompletionReplacementRange { start: 16, end: 21 })
        );
    }
}
