use super::cache::CompletionCache;
use super::command::{CommandCompletionDatabase, CompletionCandidate};

use super::framework::CompletionFrameworkKind;

use super::generator::CompletionGenerator;

use super::json_loader::JsonCompletionLoader;
use super::parser::{self, CommandLineParser, ParsedCommandLine};
use crate::completion::display::Candidate;
use crate::environment::Environment;
use anyhow::Result;
use parking_lot::{Mutex, RwLock};
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

// Pre-compiled regex for efficient whitespace splitting
static WHITESPACE_SPLIT_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\s+").unwrap());

const DEFAULT_CACHE_TTL_MS: u64 = 3000;

#[derive(Debug, Clone, Copy)]
struct CompletionRequest<'a> {
    input: &'a str,
    #[allow(dead_code)]
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

    fn _inclusive(candidates: Vec<EnhancedCandidate>) -> Self {
        Self {
            candidates,
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

    #[allow(dead_code)]
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

#[derive(Debug)]
pub struct CompletionResult {
    pub candidates: Vec<EnhancedCandidate>,
    pub framework: CompletionFrameworkKind,
}

struct CandidateAggregator<'a> {
    engine: &'a IntegratedCompletionEngine,
    max_results: usize,
    collected: Vec<EnhancedCandidate>,
    framework: Option<CompletionFrameworkKind>,
}

impl<'a> CandidateAggregator<'a> {
    fn new(engine: &'a IntegratedCompletionEngine, max_results: usize) -> Self {
        Self {
            engine,
            max_results,
            collected: Vec::new(),
            framework: None,
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
        history: Option<&Arc<parking_lot::Mutex<crate::history::FrecencyHistory>>>,
    ) -> CompletionResult {
        let candidates =
            self.engine
                .deduplicate_and_sort(self.collected, self.max_results, history);
        let framework = self
            .framework
            .unwrap_or_else(super::default_completion_framework);

        CompletionResult {
            candidates,
            framework,
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

        // Update args to use specified_arguments and options to use specified_options
        parsed.args = parsed.specified_arguments.clone();
        parsed.options = parsed.specified_options.clone();

        parsed
    }

    /// Execute integrated completion
    pub async fn complete(
        &self,
        input: &str,
        cursor_pos: usize,
        current_dir: &Path,
        max_results: usize,
        history: Option<&Arc<parking_lot::Mutex<crate::history::FrecencyHistory>>>,
    ) -> CompletionResult {
        debug!(
            "Integrated completion for: '{}' at position {} in {:?}",
            input, cursor_pos, current_dir
        );

        let request = CompletionRequest::new(input, current_dir, max_results, cursor_pos);

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
                };
            }
        }

        let mut aggregator = CandidateAggregator::new(self, request.max_results);

        let parsed_command_line = self.convert_to_parsed_command_line(input, cursor_pos);

        // 1. JSON-based command completion
        let command_collection = self.collect_command_candidates(&request, &parsed_command_line);

        if !aggregator.extend(command_collection.batch) {
            let results = aggregator.finalize(history);
            self.store_in_cache(request.input, &results.candidates, results.framework);
            return results;
        }

        // 2. Context analysis placeholder (reserved for future providers)
        let parts: Vec<&str> = WHITESPACE_SPLIT_REGEX.split(request.input).collect();
        if !parts.is_empty() {
            let _command = parts[0];
            let _args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
        }

        // 3. History-based completion (skipped when command-specific data exists)
        // History completion removed as per user request
        /*
        if !has_command_specific_data {
            let history_batch = self.collect_history_candidates(&request);
            if !aggregator.extend(history_batch) {
                let results = aggregator.finalize();
                self.store_in_cache(request.input, &results.candidates, results.framework);
                return results;
            }
        } else {
            debug!(
                "Skipping history completion as command '{}' has JSON completion data",
                parsed_command_line.command
            );
        }
        */
        let results = aggregator.finalize(history);
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

        // Check if we need to load completion for this command
        if let Some(loader) = &self.loader {
            let command_name = &parsed_command_line.command;
            let mut db = self.command_completion.lock();

            if db.get_command(command_name).is_none() {
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
        }

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
            Err(e) => {
                warn!("Failed to generate JSON completion candidates: {}", e);
                CommandCollection {
                    batch: CandidateBatch::empty(),
                }
            }
        }
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
        history: Option<&Arc<parking_lot::Mutex<crate::history::FrecencyHistory>>>,
    ) -> Vec<EnhancedCandidate> {
        // Boost priority based on history
        if let Some(history_arc) = history {
            let history = history_arc.lock();
            if let Some(store) = &history.store {
                for candidate in &mut candidates {
                    // Simple heuristic: check if candidate text appears in history items
                    // Ideally we should form the fuil command context, but for now
                    // we boost if the candidate word itself appears frequently in history
                    // OR if we can check "cmd candidate"
                    // Since we don't have easy context here, we'll try a basic approach:
                    // If candidate is a subcommand (or similar), its value is high if used often.

                    // NOTE: This is a simplification. A better approach requires passing the parsed command context.
                    // For now, let's assume `candidate.text` is significant.

                    // boost = frequency count log or similar
                    let count = store
                        .items
                        .iter()
                        .filter(|i| i.item.contains(&candidate.text)) // Loose match
                        .count();

                    if count > 0 {
                        candidate.priority += (count as u32).min(1000);
                    }
                }
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
            CandidateType::Generic => 7,
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
            CandidateType::Generic => "💡",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_enhanced_candidate_display_text() {}
}
