#![allow(dead_code)]
use super::command::CompletionCandidate;
use super::dynamic::DynamicCompletionRegistry;
use super::fuzzy::{FuzzyCompletion, SmartCompletion};
use super::generator::CompletionGenerator;
use super::history::{CompletionContext, HistoryCompletion};
use super::json_loader::JsonCompletionLoader;
use super::parser::{self, CommandLineParser, ParsedCommandLine};
use crate::completion::display::Candidate;
use anyhow::Result;
use regex::Regex;
use std::path::Path;
use tracing::{debug, warn};

// Pre-compiled regex for efficient whitespace splitting
static WHITESPACE_SPLIT_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// Integrated completion engine - integrates all completion features
pub struct IntegratedCompletionEngine {
    /// JSON-based command completion
    command_generator: Option<CompletionGenerator>,
    /// Command line parser
    parser: CommandLineParser,
    /// Existing completion systems
    fuzzy_completion: FuzzyCompletion,
    history_completion: HistoryCompletion,
    smart_completion: SmartCompletion,
    /// Dynamic completion registry
    dynamic_registry: DynamicCompletionRegistry,
}

impl IntegratedCompletionEngine {
    /// Create a new integrated completion engine
    pub fn new() -> Self {
        Self {
            command_generator: None,
            parser: CommandLineParser::new(),
            fuzzy_completion: FuzzyCompletion::new(),
            history_completion: HistoryCompletion::new(),
            smart_completion: SmartCompletion::new(),
            dynamic_registry: DynamicCompletionRegistry::with_default_handlers(),
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

        ParsedCommandLine {
            command: parsed.command.clone(),
            args: parsed.specified_arguments.clone(),
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

    /// Load history data
    pub fn load_history(&mut self, history_path: &Path) -> Result<()> {
        self.history_completion.load_history(history_path)
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

        let mut all_candidates = Vec::new();

        // 0. Check dynamic completions first (highest priority)
        let parsed_command_line = self.convert_to_parsed_command_line(input, cursor_pos);
        if self.dynamic_registry.matches(&parsed_command_line) {
            debug!("Using dynamic completion for input: '{}'", input);
            match self
                .dynamic_registry
                .generate_candidates(&parsed_command_line)
                .await
            {
                Ok(dynamic_candidates) => {
                    let enhanced_candidates = dynamic_candidates
                        .into_iter()
                        .map(|c| self.convert_to_enhanced_candidate(c, CandidateSource::Dynamic))
                        .collect::<Vec<_>>();

                    debug!(
                        "Dynamic completion generated {} candidates for '{}'",
                        enhanced_candidates.len(),
                        input
                    );

                    if !enhanced_candidates.is_empty() {
                        all_candidates.extend(enhanced_candidates);
                        return self.deduplicate_and_sort(all_candidates, max_results);
                    }
                }
                Err(e) => {
                    warn!("Failed to generate dynamic completion candidates: {}", e);
                }
            }
        }

        // 1. JSON-based command completion (if no dynamic completions)
        if let Some(ref generator) = self.command_generator {
            debug!("Using JSON completion generator for input: '{}'", input);
            let parsed = self.parser.parse(input, cursor_pos);
            debug!("Parsed command: {:?}", parsed);

            if parsed.completion_context == parser::CompletionContext::Command {
                debug!("No completion context found - skipping JSON completion");
                return all_candidates;
            }

            match generator.generate_candidates(&parsed) {
                Ok(command_candidates) => {
                    let enhanced_candidates = command_candidates
                        .into_iter()
                        .map(|c| self.convert_to_enhanced_candidate(c, CandidateSource::Command))
                        .collect::<Vec<_>>();

                    debug!(
                        "JSON completion generated {} candidates for '{}'",
                        enhanced_candidates.len(),
                        input
                    );
                    all_candidates.extend(enhanced_candidates);
                }
                Err(e) => {
                    warn!("Failed to generate JSON completion candidates: {}", e);
                }
            }
        } else {
            debug!("No JSON completion generator available - skipping JSON completion");
        }

        // 2. Existing context completion
        let parts: Vec<&str> = WHITESPACE_SPLIT_REGEX.split(input).collect();
        if !parts.is_empty() {
            let _command = parts[0];
            let _args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
        }

        // 3. History-based completion
        let context = CompletionContext::new(current_dir.to_string_lossy().to_string());
        let history_candidates = self.history_completion.complete_command(input, &context);
        let enhanced_candidates = history_candidates
            .into_iter()
            .map(|c| self.convert_legacy_candidate(c, CandidateSource::History))
            .collect::<Vec<_>>();

        debug!("Generated {} history candidates", enhanced_candidates.len());
        all_candidates.extend(enhanced_candidates);

        // 4. Deduplication and sorting
        self.deduplicate_and_sort(all_candidates, max_results)
    }

    /// Convert CompletionCandidate to EnhancedCandidate
    fn convert_to_enhanced_candidate(
        &self,
        candidate: CompletionCandidate,
        source: CandidateSource,
    ) -> EnhancedCandidate {
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
            source,
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
                CandidateSource::Dynamic => 120, // Highest priority
                CandidateSource::Command => 100,
                CandidateSource::Context => 80,
                CandidateSource::History => 60,
            },
            source,
        }
    }

    /// Convert EnhancedCandidate list to Candidate list for skim display
    pub fn to_candidates(&self, enhanced_candidates: Vec<EnhancedCandidate>) -> Vec<Candidate> {
        enhanced_candidates
            .into_iter()
            .map(|ec| ec.to_candidate())
            .collect()
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

impl Default for IntegratedCompletionEngine {
    fn default() -> Self {
        Self::new()
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
    /// Candidate source
    pub source: CandidateSource,
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

    /// Get display color
    pub fn color(&self) -> crossterm::style::Color {
        use crossterm::style::Color;
        match self {
            CandidateType::SubCommand => Color::Green,
            CandidateType::LongOption | CandidateType::ShortOption => Color::Blue,
            CandidateType::Argument => Color::Yellow,
            CandidateType::File => Color::White,
            CandidateType::Directory => Color::Cyan,
            CandidateType::Generic => Color::Grey,
        }
    }
}

/// Candidate source
#[derive(Debug, Clone, PartialEq)]
pub enum CandidateSource {
    /// Command completion system
    Command,
    /// Context completion
    Context,
    /// History completion
    History,
    /// Dynamic completion
    Dynamic,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_integrated_completion_engine_creation() {
        let engine = IntegratedCompletionEngine::new();
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
            source: CandidateSource::Command,
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
            source: CandidateSource::Command,
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
            source: CandidateSource::Command,
        };

        let display_text = enhanced_candidate.get_display_text();
        assert!(display_text.contains("🔧"));
        assert!(display_text.contains("--verbose"));
        assert!(display_text.contains("Show detailed output"));
    }
}
