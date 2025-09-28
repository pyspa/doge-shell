use super::cache::CompletionCache;
use super::command::CompletionCandidate;
use super::dynamic::DynamicCompletionRegistry;

use super::generator::CompletionGenerator;
use super::history::{CompletionContext, HistoryCompletion};
use super::json_loader::JsonCompletionLoader;
use super::parser::{self, CommandLineParser, ParsedCommandLine};
use crate::completion::display::Candidate;
use crate::environment::Environment;
use anyhow::Result;
use parking_lot::RwLock;
use regex::Regex;
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
    current_dir: &'a Path,
    max_results: usize,
}

impl<'a> CompletionRequest<'a> {
    fn new(input: &'a str, current_dir: &'a Path, max_results: usize) -> Self {
        Self {
            input,
            current_dir,
            max_results,
        }
    }
}

#[derive(Debug, Default)]
struct CandidateBatch {
    candidates: Vec<EnhancedCandidate>,
    exclusive: bool,
}

impl CandidateBatch {
    fn empty() -> Self {
        Self {
            candidates: Vec::new(),
            exclusive: false,
        }
    }

    fn inclusive(candidates: Vec<EnhancedCandidate>) -> Self {
        Self {
            candidates,
            exclusive: false,
        }
    }

    fn exclusive(candidates: Vec<EnhancedCandidate>) -> Self {
        Self {
            candidates,
            exclusive: true,
        }
    }
}

#[derive(Debug)]
struct CommandCollection {
    batch: CandidateBatch,
    has_command_specific_data: bool,
}

impl CommandCollection {
    fn empty() -> Self {
        Self {
            batch: CandidateBatch::empty(),
            has_command_specific_data: false,
        }
    }
}

struct CandidateAggregator<'a> {
    engine: &'a IntegratedCompletionEngine,
    max_results: usize,
    collected: Vec<EnhancedCandidate>,
}

impl<'a> CandidateAggregator<'a> {
    fn new(engine: &'a IntegratedCompletionEngine, max_results: usize) -> Self {
        Self {
            engine,
            max_results,
            collected: Vec::new(),
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
        !batch.exclusive
    }

    fn finalize(self) -> Vec<EnhancedCandidate> {
        self.engine
            .deduplicate_and_sort(self.collected, self.max_results)
    }
}

/// Integrated completion engine - integrates all completion features
pub struct IntegratedCompletionEngine {
    /// JSON-based command completion
    command_generator: Option<CompletionGenerator>,
    /// Command line parser
    parser: CommandLineParser,
    history_completion: HistoryCompletion,
    /// Dynamic completion registry
    dynamic_registry: DynamicCompletionRegistry,
    /// Short lived completion cache
    cache: CompletionCache<EnhancedCandidate>,

    /// Shell environment (for dynamic completion)
    pub environment: Arc<RwLock<Environment>>,
}

impl IntegratedCompletionEngine {
    /// Create a new integrated completion engine
    pub fn new(environment: Arc<RwLock<Environment>>) -> Self {
        Self {
            command_generator: None,
            parser: CommandLineParser::new(),
            history_completion: HistoryCompletion::new(),
            dynamic_registry: DynamicCompletionRegistry::with_default_handlers(),
            cache: CompletionCache::new(Duration::from_millis(DEFAULT_CACHE_TTL_MS)),
            environment,
        }
    }

    /// Initialize JSON completion data
    pub fn initialize_command_completion(&mut self) -> Result<()> {
        debug!("Initializing command completion system...");

        debug!("Creating JsonCompletionLoader...");
        let loader = JsonCompletionLoader::new();

        debug!("Loading completion database...");
        match loader.load_database() {
            Ok(database) => {
                let command_count = database.len();
                debug!("Loaded completion database with {} commands", command_count);

                if command_count > 0 {
                    debug!("Creating CompletionGenerator with database...");
                    self.command_generator = Some(CompletionGenerator::new(database));
                    debug!(
                        "Command completion initialized successfully with {} commands",
                        command_count
                    );

                    // Debug: List loaded commands
                    if let Some(ref generator) = self.command_generator {
                        debug!(
                            "Available commands in database: {:?}",
                            generator.get_available_commands()
                        );
                    }
                } else {
                    warn!("No command completion data found - completion database is empty");
                }
            }
            Err(e) => {
                warn!("Failed to load command completion data: {}", e);
                return Err(e);
            }
        }

        Ok(())
    }

    /// Convert ParsedCommand to ParsedCommandLine for dynamic completion
    fn convert_to_parsed_command_line(&self, input: &str, cursor_pos: usize) -> ParsedCommandLine {
        let parsed = self.parser.parse(input, cursor_pos);

        // For dynamic completion, we need to combine arguments and options in the order they appear
        // This is important for commands like "sudo pacman -S" where we need to match the sequence
        let mut args = Vec::new();
        args.extend(parsed.specified_arguments.clone());

        let mut opts = Vec::new();
        // For pacman completion specifically, we need to handle the case where options come after arguments
        // In "sudo pacman -S", "-S" is an option that comes after the argument "pacman"
        // But for dynamic completion matching, we want to see ["pacman", "-S"]
        opts.extend(parsed.specified_options.clone());

        ParsedCommandLine {
            command: parsed.command.clone(),
            args,
            options: opts,
            current_arg: Some(parsed.current_token.clone()),
            completion_context: match parsed.completion_context {
                parser::CompletionContext::Command => super::parser::CompletionContext::Command,
                parser::CompletionContext::SubCommand => {
                    super::parser::CompletionContext::SubCommand
                }
                parser::CompletionContext::ShortOption | parser::CompletionContext::LongOption => {
                    super::parser::CompletionContext::ShortOption
                }
                parser::CompletionContext::OptionValue { .. } => {
                    super::parser::CompletionContext::OptionValue {
                        option_name: "".to_string(),
                        value_type: None,
                    }
                }
                parser::CompletionContext::Argument { .. } => {
                    super::parser::CompletionContext::Argument {
                        arg_index: 0,
                        arg_type: None,
                    }
                }
                parser::CompletionContext::Unknown => super::parser::CompletionContext::Unknown,
            },
            cursor_index: cursor_pos,
        }
    }

    /// Execute integrated completion
    pub async fn complete(
        &self,
        input: &str,
        cursor_pos: usize,
        current_dir: &Path,
        max_results: usize,
    ) -> Vec<EnhancedCandidate> {
        debug!(
            "Integrated completion for: '{}' at position {} in {:?}",
            input, cursor_pos, current_dir
        );

        let request = CompletionRequest::new(input, current_dir, max_results);

        if !request.input.is_empty()
            && let Some(hit) = self.cache.lookup(request.input)
        {
            debug!(
                "cache hit for '{}' (key: '{}', exact: {})",
                request.input, hit.key, hit.exact
            );

            if hit.exact || !hit.candidates.is_empty() {
                self.cache.extend_ttl(&hit.key);
                return hit.candidates;
            }
        }

        let mut aggregator = CandidateAggregator::new(self, request.max_results);

        // 0. Dynamic completion (highest priority, exclusive when available)
        let parsed_command_line = self.convert_to_parsed_command_line(input, cursor_pos);
        let dynamic_batch = self.collect_dynamic_candidates(&request, &parsed_command_line);
        if !aggregator.extend(dynamic_batch) {
            let results = aggregator.finalize();
            self.store_in_cache(request.input, &results);
            return results;
        }

        // 1. JSON-based command completion
        let parsed_command = self.parser.parse(input, cursor_pos);
        let command_collection = self.collect_command_candidates(&request, &parsed_command);
        let has_command_specific_data = command_collection.has_command_specific_data;
        if !aggregator.extend(command_collection.batch) {
            let results = aggregator.finalize();
            self.store_in_cache(request.input, &results);
            return results;
        }

        // 2. Context analysis placeholder (reserved for future providers)
        let parts: Vec<&str> = WHITESPACE_SPLIT_REGEX.split(request.input).collect();
        if !parts.is_empty() {
            let _command = parts[0];
            let _args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
        }

        // 3. History-based completion (skipped when command-specific data exists)
        if !has_command_specific_data {
            let history_batch = self.collect_history_candidates(&request);
            if !aggregator.extend(history_batch) {
                let results = aggregator.finalize();
                self.store_in_cache(request.input, &results);
                return results;
            }
        } else {
            debug!(
                "Skipping history completion as command '{}' has JSON completion data",
                parsed_command.command
            );
        }
        let results = aggregator.finalize();
        self.store_in_cache(request.input, &results);
        results
    }

    fn collect_dynamic_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &ParsedCommandLine,
    ) -> CandidateBatch {
        if !self.dynamic_registry.matches(parsed_command_line) {
            return CandidateBatch::empty();
        }

        debug!("Using dynamic completion for input: '{}'", request.input);
        match self
            .dynamic_registry
            .generate_candidates(parsed_command_line)
        {
            Ok(dynamic_candidates) => {
                let enhanced_candidates = dynamic_candidates
                    .into_iter()
                    .map(|c| self.convert_to_enhanced_candidate(c))
                    .collect::<Vec<_>>();

                debug!(
                    "Dynamic completion generated {} candidates for '{}'",
                    enhanced_candidates.len(),
                    request.input
                );

                if enhanced_candidates.is_empty() {
                    CandidateBatch::empty()
                } else {
                    CandidateBatch::exclusive(enhanced_candidates)
                }
            }
            Err(e) => {
                warn!("Failed to generate dynamic completion candidates: {}", e);
                CandidateBatch::empty()
            }
        }
    }

    fn collect_command_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command: &parser::ParsedCommand,
    ) -> CommandCollection {
        if parsed_command.completion_context == parser::CompletionContext::Command {
            debug!("No completion context found - skipping JSON completion");
            return CommandCollection::empty();
        }

        let Some(generator) = &self.command_generator else {
            debug!("No JSON completion generator available - skipping JSON completion");
            return CommandCollection::empty();
        };

        debug!(
            "Using JSON completion generator for input: '{}'",
            request.input
        );

        let has_command_specific_data = generator.has_command_completion(&parsed_command.command);

        match generator.generate_candidates(parsed_command) {
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
                    batch: CandidateBatch::inclusive(enhanced_candidates),
                    has_command_specific_data,
                }
            }
            Err(e) => {
                warn!("Failed to generate JSON completion candidates: {}", e);
                CommandCollection {
                    batch: CandidateBatch::empty(),
                    has_command_specific_data,
                }
            }
        }
    }

    fn collect_history_candidates(&self, request: &CompletionRequest) -> CandidateBatch {
        let context = CompletionContext::new(request.current_dir.to_string_lossy().to_string());
        let history_candidates = self
            .history_completion
            .complete_command(request.input, &context);
        let enhanced_candidates = history_candidates
            .into_iter()
            .map(|c| self.convert_legacy_candidate(c, CandidateSource::History))
            .collect::<Vec<_>>();

        debug!("Generated {} history candidates", enhanced_candidates.len());

        CandidateBatch::inclusive(enhanced_candidates)
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

    /// Convert existing Candidate to EnhancedCandidate
    fn convert_legacy_candidate(
        &self,
        candidate: Candidate,
        source: CandidateSource,
    ) -> EnhancedCandidate {
        let (text, description, candidate_type) = match candidate {
            Candidate::Item(text, desc) => (text, Some(desc), CandidateType::Generic),
            Candidate::File { path, is_dir } => (
                path,
                None,
                if is_dir {
                    CandidateType::Directory
                } else {
                    CandidateType::File
                },
            ),
            Candidate::Path(path) => (path, None, CandidateType::File),
            Candidate::Basic(text) => (text, None, CandidateType::Generic),
            Candidate::Command { name, description } => {
                (name, Some(description), CandidateType::SubCommand)
            }
            Candidate::History { command, .. } => (command, None, CandidateType::Generic),
            Candidate::GitBranch { name, .. } => (name, None, CandidateType::Generic),
            Candidate::Option { name, description } => {
                (name, Some(description), CandidateType::LongOption)
            }
        };

        EnhancedCandidate {
            text,
            description,
            candidate_type,
            priority: match source {
                CandidateSource::History => 60,
            },
        }
    }

    /// Convert EnhancedCandidate list to Candidate list for skim display
    pub fn to_candidates(&self, enhanced_candidates: Vec<EnhancedCandidate>) -> Vec<Candidate> {
        enhanced_candidates
            .into_iter()
            .map(|ec| ec.to_candidate())
            .collect()
    }

    fn store_in_cache(&self, key: &str, candidates: &[EnhancedCandidate]) {
        if key.is_empty() {
            return;
        }
        debug!("cache set for '{}'. len: {}", key, candidates.len());

        self.cache.set(key.to_string(), candidates.to_vec());
    }

    /// Deduplication and sorting
    fn deduplicate_and_sort(
        &self,
        mut candidates: Vec<EnhancedCandidate>,
        max_results: usize,
    ) -> Vec<EnhancedCandidate> {
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

    /// Get display text with icon and description for skim
    #[allow(dead_code)]
    pub fn get_display_text(&self) -> String {
        let icon = self.candidate_type.icon();
        match &self.description {
            Some(desc) => format!("{} {:<30} {}", icon, self.text, desc),
            None => format!("{} {}", icon, self.text),
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

/// Candidate source
#[derive(Debug, Clone, PartialEq)]
pub enum CandidateSource {
    History,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_integrated_completion_engine_creation() {
        let engine = IntegratedCompletionEngine::new(Environment::new());
        assert!(engine.command_generator.is_none());
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
    fn test_enhanced_candidate_display_text() {
        let enhanced_candidate = EnhancedCandidate {
            text: "--verbose".to_string(),
            description: Some("Show detailed output".to_string()),
            candidate_type: CandidateType::LongOption,
            priority: 80,
        };

        let display_text = enhanced_candidate.get_display_text();
        assert!(display_text.contains("🔧"));
        assert!(display_text.contains("--verbose"));
        assert!(display_text.contains("Show detailed output"));
    }
}
