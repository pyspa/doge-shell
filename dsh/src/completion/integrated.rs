use super::cache::CompletionCache;
use super::command::{
    ArgumentType, CommandCompletionDatabase, CommandOption, CompletionCandidate, SubCommand,
};
use super::dynamic::DynamicCompletionProvider;

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
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

const DEFAULT_CACHE_TTL_MS: u64 = 3000;
const HISTORY_BOOST_SCAN_LIMIT: usize = 2000;
const HISTORY_BOOST_SCORE_CAP: u32 = 5000;
const JS_TASK_SOURCES: &[&str] = &["npm", "pnpm", "yarn", "bun"];
const DENO_TASK_SOURCES: &[&str] = &["deno"];
const JUST_TASK_SOURCES: &[&str] = &["just"];
const MAKE_TASK_SOURCES: &[&str] = &["make"];

type DynamicProviderFn = for<'a> fn(
    &IntegratedCompletionEngine,
    &CompletionRequest<'a>,
    &ParsedCommandLine,
) -> Vec<EnhancedCandidate>;

struct DynamicProviderSpec {
    command: &'static str,
    collect: DynamicProviderFn,
}

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
        let uses_dynamic_completion = is_dynamic_completion_command(&parsed_command_line.command);

        if !uses_dynamic_completion
            && !request.input.is_empty()
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
            if !uses_dynamic_completion {
                self.store_in_cache(request.input, &results.candidates, results.framework);
            }
            return results;
        }

        // 2. JSON-based command completion
        let command_collection = self.collect_command_candidates(&request, &parsed_command_line);
        if !aggregator.extend(command_collection.batch) {
            let mut results = aggregator.finalize(history);
            results.replacement_range = replacement_range;
            if !uses_dynamic_completion {
                self.store_in_cache(request.input, &results.candidates, results.framework);
            }
            return results;
        }

        // 3. External completer fallback
        let external_batch = self.collect_external_candidates(&request, &parsed_command_line);
        if !aggregator.extend(external_batch) {
            let mut results = aggregator.finalize(history);
            results.replacement_range = replacement_range;
            if !uses_dynamic_completion {
                self.store_in_cache(request.input, &results.candidates, results.framework);
            }
            return results;
        }

        // 4. Optional fish-compatible fallback. This is deliberately lower priority than
        // project-aware dynamic and JSON completion, but it can supply broad fish-style
        // candidates for commands without built-in definitions.
        let fish_batch = self.collect_fish_fallback_candidates(&request, &parsed_command_line);
        if !aggregator.extend(fish_batch) {
            let mut results = aggregator.finalize(history);
            results.replacement_range = replacement_range;
            if !uses_dynamic_completion {
                self.store_in_cache(request.input, &results.candidates, results.framework);
            }
            return results;
        }

        let mut results = aggregator.finalize(history);
        results.replacement_range = replacement_range;
        if !uses_dynamic_completion {
            self.store_in_cache(request.input, &results.candidates, results.framework);
        }
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
        let mut candidates = DYNAMIC_PROVIDER_SPECS
            .iter()
            .find(|provider| provider.command == parsed_command_line.command)
            .map(|provider| (provider.collect)(self, request, parsed_command_line))
            .unwrap_or_default();
        candidates.extend(self.collect_declared_dynamic_candidates(
            request,
            parsed_command_line,
            false,
        ));

        if candidates.is_empty() {
            CandidateBatch::empty()
        } else {
            CandidateBatch::inclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
        }
    }

    fn collect_declared_dynamic_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
        cached_only: bool,
    ) -> Vec<EnhancedCandidate> {
        let Some(ArgumentType::Dynamic { provider, scope }) =
            self.argument_type_for_completion_context(parsed_command_line)
        else {
            return Vec::new();
        };

        self.dynamic.collect_declared_dynamic_candidates(
            &provider,
            scope.as_deref(),
            parsed_command_line,
            request.current_dir,
            cached_only,
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
        match parsed_command_line.command.as_str() {
            "git" => self
                .dynamic
                .collect_git_candidates_cached(parsed_command_line, request.current_dir),
            "docker" => self
                .dynamic
                .collect_docker_candidates_cached(parsed_command_line, request.current_dir),
            "kubectl" => self
                .dynamic
                .collect_kubectl_candidates_cached(parsed_command_line, request.current_dir),
            "cargo" => self
                .dynamic
                .collect_cargo_candidates_cached(parsed_command_line, request.current_dir),
            "systemctl" => self
                .dynamic
                .collect_systemctl_candidates_cached(parsed_command_line, request.current_dir),
            "journalctl" => self
                .dynamic
                .collect_journalctl_candidates_cached(parsed_command_line, request.current_dir),
            "ssh" => self.dynamic.collect_ssh_host_candidates_cached(
                parsed_command_line,
                request.current_dir,
                "ssh",
            ),
            "scp" => self.dynamic.collect_ssh_host_candidates_cached(
                parsed_command_line,
                request.current_dir,
                "scp",
            ),
            "rsync" => self.dynamic.collect_ssh_host_candidates_cached(
                parsed_command_line,
                request.current_dir,
                "rsync",
            ),
            "tmux" => self
                .dynamic
                .collect_tmux_candidates_cached(parsed_command_line, request.current_dir),
            "screen" => self
                .dynamic
                .collect_screen_candidates_cached(parsed_command_line, request.current_dir),
            "pgrep" => self
                .dynamic
                .collect_process_name_candidates_cached(parsed_command_line, "pgrep"),
            "pkill" => self
                .dynamic
                .collect_process_name_candidates_cached(parsed_command_line, "pkill"),
            "pip" => self.dynamic.collect_pip_candidates_cached(
                parsed_command_line,
                request.current_dir,
                "pip",
            ),
            "pip3" => self.dynamic.collect_pip_candidates_cached(
                parsed_command_line,
                request.current_dir,
                "pip3",
            ),
            "rustup" => self
                .dynamic
                .collect_rustup_candidates_cached(parsed_command_line, request.current_dir),
            "gh" => self
                .dynamic
                .collect_gh_candidates_cached(parsed_command_line, request.current_dir),
            "nmcli" => self
                .dynamic
                .collect_nmcli_candidates_cached(parsed_command_line, request.current_dir),
            "pacman" => self
                .dynamic
                .collect_pacman_candidates_cached(parsed_command_line, request.current_dir),
            "mount" => self
                .dynamic
                .collect_mount_candidates_cached(parsed_command_line, request.current_dir),
            "umount" => self
                .dynamic
                .collect_umount_candidates_cached(parsed_command_line, request.current_dir),
            "modprobe" => self
                .dynamic
                .collect_modprobe_candidates_cached(parsed_command_line),
            "tcpdump" => self
                .dynamic
                .collect_tcpdump_candidates_cached(parsed_command_line),
            "npm" | "pnpm" => {
                let completes_dependency = match parsed_command_line.command.as_str() {
                    "pnpm" => {
                        leading_completion_words_match(parsed_command_line, &["remove"])
                            || leading_completion_words_match(parsed_command_line, &["update"])
                            || leading_completion_words_match(parsed_command_line, &["why"])
                    }
                    _ => {
                        leading_completion_words_match(parsed_command_line, &["uninstall"])
                            || leading_completion_words_match(parsed_command_line, &["update"])
                    }
                };
                if completes_dependency {
                    self.dynamic.collect_js_dependency_candidates(
                        parsed_command_line,
                        request.current_dir,
                        parsed_command_line.command.as_str(),
                        true,
                    )
                } else {
                    Vec::new()
                }
            }
            "yarn" => {
                if leading_completion_words_match(parsed_command_line, &["remove"])
                    || leading_completion_words_match(parsed_command_line, &["why"])
                    || leading_completion_words_match(parsed_command_line, &["upgrade"])
                {
                    self.dynamic.collect_js_dependency_candidates(
                        parsed_command_line,
                        request.current_dir,
                        "yarn",
                        true,
                    )
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
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
            return self.dynamic.collect_declared_dynamic_candidates(
                &provider,
                scope.as_deref(),
                parsed_command_line,
                current_dir,
                true,
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

pub(super) fn matches_prefix(current_token: &str, value: &str) -> bool {
    current_token.is_empty()
        || value.starts_with(current_token)
        || super::fuzzy_match_score(value, current_token).is_some()
}

fn is_dynamic_completion_command(command: &str) -> bool {
    DYNAMIC_PROVIDER_SPECS
        .iter()
        .any(|provider| provider.command == command)
}

fn collect_task_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_task_candidates(parsed, request.current_dir)
}

fn collect_pm_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine.collect_pm_candidates(parsed)
}

fn collect_pj_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine.collect_pj_candidates(parsed)
}

fn collect_mcp_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine.collect_mcp_candidates(parsed)
}

fn collect_git_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_git_candidates(parsed, request.current_dir)
}

fn collect_docker_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_docker_candidates(parsed, request.current_dir)
}

fn collect_kubectl_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_kubectl_candidates(parsed, request.current_dir)
}

fn collect_cargo_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_cargo_candidates(parsed, request.current_dir)
}

fn collect_systemctl_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_systemctl_candidates(parsed, request.current_dir)
}

fn collect_journalctl_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_journalctl_candidates(parsed, request.current_dir)
}

fn collect_ssh_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_ssh_host_candidates(parsed, request.current_dir, "ssh")
}

fn collect_scp_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_ssh_host_candidates(parsed, request.current_dir, "scp")
}

fn collect_rsync_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_ssh_host_candidates(parsed, request.current_dir, "rsync")
}

fn collect_tmux_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_tmux_candidates(parsed, request.current_dir)
}

fn collect_screen_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_screen_candidates(parsed, request.current_dir)
}

fn collect_pgrep_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_process_name_candidates(parsed, "pgrep")
}

fn collect_pkill_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_process_name_candidates(parsed, "pkill")
}

fn collect_pip_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_pip_candidates(parsed, request.current_dir, "pip")
}

fn collect_pip3_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_pip_candidates(parsed, request.current_dir, "pip3")
}

fn collect_rustup_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_rustup_candidates(parsed, request.current_dir)
}

fn collect_gh_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_gh_candidates(parsed, request.current_dir)
}

fn collect_nmcli_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_nmcli_candidates(parsed, request.current_dir)
}

fn collect_pacman_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_pacman_candidates(parsed, request.current_dir)
}

fn collect_mount_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_mount_candidates(parsed, request.current_dir)
}

fn collect_umount_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine
        .dynamic
        .collect_umount_candidates(parsed, request.current_dir)
}

fn collect_modprobe_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine.dynamic.collect_modprobe_candidates(parsed)
}

fn collect_tcpdump_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    _request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine.dynamic.collect_tcpdump_candidates(parsed)
}

fn collect_npm_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    let mut candidates = engine.collect_package_run_candidates(parsed, request.current_dir);
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
            false,
        ));
    }
    candidates
}

fn collect_yarn_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    let mut candidates = engine.collect_yarn_script_candidates(parsed, request.current_dir);
    if leading_completion_words_match(parsed, &["remove"])
        || leading_completion_words_match(parsed, &["why"])
        || leading_completion_words_match(parsed, &["upgrade"])
    {
        candidates.extend(engine.dynamic.collect_js_dependency_candidates(
            parsed,
            request.current_dir,
            "yarn",
            false,
        ));
    }
    candidates
}

fn collect_deno_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine.collect_deno_task_candidates(parsed, request.current_dir)
}

fn collect_just_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine.collect_top_level_task_candidates(parsed, request.current_dir, JUST_TASK_SOURCES)
}

fn collect_make_dynamic_candidates(
    engine: &IntegratedCompletionEngine,
    request: &CompletionRequest<'_>,
    parsed: &ParsedCommandLine,
) -> Vec<EnhancedCandidate> {
    engine.collect_top_level_task_candidates(parsed, request.current_dir, MAKE_TASK_SOURCES)
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

    async fn wait_for_candidate(
        engine: &IntegratedCompletionEngine,
        input: &str,
        cwd: &Path,
        expected: &str,
    ) -> CompletionResult {
        let start = std::time::Instant::now();
        loop {
            let result = engine.complete(input, input.len(), cwd, 50, None).await;
            if result
                .candidates
                .iter()
                .any(|candidate| candidate.text == expected)
            {
                return result;
            }
            let last_candidates: Vec<_> = result
                .candidates
                .iter()
                .map(|candidate| candidate.text.clone())
                .collect();
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "timed out waiting for completion candidate {expected} for input {input}; last candidates: {last_candidates:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn write_executable_script(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn engine_with_path(bin_dir: &Path) -> IntegratedCompletionEngine {
        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![bin_dir.display().to_string()];
            env.clear_command_cache();
        }
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();
        engine
    }

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
    async fn pytest_string_option_value_does_not_fallback_to_files_but_next_arg_does() {
        let dir = tempdir().unwrap();
        let test_file = dir.path().join("tests_alpha.py");
        fs::write(&test_file, "").unwrap();
        let file_prefix = dir.path().join("tests_").to_string_lossy().to_string();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let option_value_input = format!("pytest -k {file_prefix}");
        let option_value_result = engine
            .complete(
                &option_value_input,
                option_value_input.len(),
                dir.path(),
                50,
                None,
            )
            .await;
        assert!(
            !option_value_result
                .candidates
                .iter()
                .any(|candidate| candidate.text == test_file.to_string_lossy()),
            "String option values must not fallback to file candidates: {:?}",
            option_value_result.candidates
        );

        let positional_input = format!("pytest -k expr {file_prefix}");
        let positional_result = engine
            .complete(
                &positional_input,
                positional_input.len(),
                dir.path(),
                50,
                None,
            )
            .await;
        assert!(
            positional_result
                .candidates
                .iter()
                .any(|candidate| candidate.text == test_file.to_string_lossy()),
            "positional pytest arguments should still complete files: {:?}",
            positional_result.candidates
        );
    }

    #[tokio::test]
    async fn double_dash_allows_following_file_argument_completion() {
        let dir = tempdir().unwrap();
        let test_file = dir.path().join("alpha.py");
        fs::write(&test_file, "").unwrap();
        let file_prefix = dir.path().join("alp").to_string_lossy().to_string();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = format!("pytest -- {file_prefix}");
        let result = engine
            .complete(&input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == test_file.to_string_lossy()),
            "expected file after -- to complete as positional argument: {:?}",
            result.candidates
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
        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        let _ = wait_for_candidate(&engine, input, dir.path(), "feature/test-branch").await;
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
        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        let result = wait_for_candidate(&engine, input, dir.path(), "dev-cluster").await;
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
        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        let result = wait_for_candidate(&engine, input, dir.path(), "dev-cluster").await;
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
        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        let result = wait_for_candidate(&engine, input, dir.path(), "dev-namespace").await;
        assert_eq!(
            result.replacement_range,
            Some(CompletionReplacementRange { start: 10, end: 12 })
        );
    }

    #[test]
    fn ghost_completion_uses_json_subcommands() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("checkout.txt"), "").unwrap();
        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "git che";
        let ghost = engine.ghost_completion(input, input.len(), dir.path(), None);

        assert_eq!(ghost.as_deref(), Some("git checkout"));
    }

    #[test]
    fn ghost_completion_uses_json_subcommands_for_cargo() {
        let dir = tempdir().unwrap();
        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "cargo bu";
        let ghost = engine.ghost_completion(input, input.len(), dir.path(), None);

        assert_eq!(ghost.as_deref(), Some("cargo build"));
    }

    #[test]
    fn ghost_completion_uses_json_choice_option_values() {
        let dir = tempdir().unwrap();
        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "ps --sort=me";
        let ghost = engine.ghost_completion(input, input.len(), dir.path(), None);

        assert_eq!(ghost.as_deref(), Some("ps --sort=mem"));
    }

    #[tokio::test]
    async fn ghost_completion_uses_cached_kubectl_context() {
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
        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        let _ = wait_for_candidate(&engine, input, dir.path(), "dev-cluster").await;

        let ghost = engine.ghost_completion(input, input.len(), dir.path(), None);
        assert_eq!(ghost.as_deref(), Some("kubectl --context=dev-cluster"));
    }

    #[tokio::test]
    async fn ghost_completion_uses_cached_json_declared_dynamic_values_only() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let counter = dir.path().join("git-count");
        write_executable_script(
            &bin_dir.join("git"),
            &format!(
                "#!/bin/sh\ncount_file=\"{}\"\ncount=0\nif [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$count_file\"\nif [ \"$1\" = \"stash\" ] && [ \"$2\" = \"list\" ]; then printf 'stash@{{0}}: WIP on main\\n'; fi\n",
                counter.display()
            ),
        );

        let engine = engine_with_path(&bin_dir);
        let input = "git stash pop stash";

        assert_eq!(
            engine.ghost_completion(input, input.len(), dir.path(), None),
            None
        );
        assert!(
            !counter.exists(),
            "ghost completion must not run uncached JSON-declared dynamic providers"
        );

        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        let _ = wait_for_candidate(&engine, input, dir.path(), "stash@{0}").await;

        assert_eq!(
            engine.ghost_completion(input, input.len(), dir.path(), None),
            Some("git stash pop stash@{0}".to_string())
        );
    }

    #[tokio::test]
    async fn ghost_completion_uses_cached_new_local_dynamic_providers_only() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let counter = dir.path().join("busctl-count");
        write_executable_script(
            &bin_dir.join("busctl"),
            &format!(
                "#!/bin/sh\ncount_file=\"{}\"\ncount=0\nif [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$count_file\"\nif [ \"$1\" = \"list\" ]; then printf 'org.freedesktop.login1 1 systemd root - - - Login\\n'; fi\n",
                counter.display()
            ),
        );

        let engine = engine_with_path(&bin_dir);
        let input = "busctl introspect org.";

        assert_eq!(
            engine.ghost_completion(input, input.len(), dir.path(), None),
            None
        );
        assert!(
            !counter.exists(),
            "ghost completion must not run uncached local dynamic providers"
        );

        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        let _ = wait_for_candidate(&engine, input, dir.path(), "org.freedesktop.login1").await;

        assert_eq!(
            engine.ghost_completion(input, input.len(), dir.path(), None),
            Some("busctl introspect org.freedesktop.login1".to_string())
        );
    }

    #[tokio::test]
    async fn npm_run_completes_package_scripts() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join("should-not-exist");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "build": "vite build", "test": "vitest" } }"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("Makefile"),
            format!("$(shell touch {})\nall:\n\t@true\n", marker.display()),
        )
        .unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "npm run bu";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "build"),
            "expected npm script completion in {:?}",
            result.candidates
        );
        assert!(
            !marker.exists(),
            "npm run completion must not invoke Makefile discovery"
        );
    }

    #[tokio::test]
    async fn pnpm_run_completes_package_scripts() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "bundle": "vite build", "test": "vitest" } }"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "pnpm run bun";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "bundle"),
            "expected pnpm script completion in {:?}",
            result.candidates
        );
    }

    #[tokio::test]
    async fn npm_run_completes_package_scripts_even_when_lockfile_selects_another_manager() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "build": "vite build", "test": "vitest" } }"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "npm run bu";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "build"),
            "expected npm script completion in {:?}",
            result.candidates
        );
    }

    #[tokio::test]
    async fn yarn_completes_package_scripts_without_run_subcommand() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "bundle": "vite build", "test": "vitest" } }"#,
        )
        .unwrap();
        fs::write(dir.path().join("yarn.lock"), "").unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "yarn bun";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "bundle"),
            "expected yarn script completion in {:?}",
            result.candidates
        );
    }

    #[tokio::test]
    async fn ghost_completion_uses_cached_json_declared_project_tasks() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "build": "bun build ./src/index.ts", "test": "bun test" } }"#,
        )
        .unwrap();
        fs::write(dir.path().join("bun.lockb"), "").unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "bun run bu";
        assert_eq!(
            engine.ghost_completion(input, input.len(), dir.path(), None),
            None
        );

        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "build"),
            "expected bun script completion in {:?}",
            result.candidates
        );

        assert_eq!(
            engine.ghost_completion(input, input.len(), dir.path(), None),
            Some("bun run build".to_string())
        );
    }

    #[tokio::test]
    async fn deno_task_completes_deno_tasks() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("deno.json"),
            r#"{ "tasks": { "build": "deno run build.ts", "test": "deno test" } }"#,
        )
        .unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "deno task bu";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "build"),
            "expected deno task completion in {:?}",
            result.candidates
        );
    }

    #[tokio::test]
    async fn just_completes_project_recipes() {
        if std::process::Command::new("just")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Justfile"),
            "test-recipe:\n\t@true\nbuild-recipe:\n\t@true\n",
        )
        .unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "just bu";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "build-recipe"),
            "expected just recipe completion in {:?}",
            result.candidates
        );
    }

    #[tokio::test]
    async fn make_completes_project_targets() {
        if std::process::Command::new("make")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Makefile"),
            "test-target:\n\t@true\nbuild-target:\n\t@true\n",
        )
        .unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "make te";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "test-target"),
            "expected make target completion in {:?}",
            result.candidates
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

    #[tokio::test]
    async fn fish_fallback_merges_below_json_candidates() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        write_executable_script(
            &bin_dir.join("fish"),
            "#!/bin/sh\nprintf 'checkout\\tFish checkout\\nche-fish-only\\tFish only\\n'\n",
        );

        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![bin_dir.display().to_string()];
            env.variables
                .insert("DSH_COMPLETION_FISH_FALLBACK".to_string(), "1".to_string());
            env.clear_command_cache();
        }
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "git che";
        let started = std::time::Instant::now();
        let result = loop {
            let result = engine
                .complete(input, input.len(), dir.path(), 200, None)
                .await;
            if result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "che-fish-only")
            {
                break result;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "expected unique fish fallback candidate to be merged"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };

        let checkout = result
            .candidates
            .iter()
            .find(|candidate| candidate.text == "checkout")
            .expect("expected git checkout from JSON completion");
        assert_eq!(
            checkout.description.as_deref(),
            Some("Switch branches or restore working tree files")
        );
        let fish_candidate = result
            .candidates
            .iter()
            .find(|candidate| candidate.text == "che-fish-only")
            .unwrap();
        assert_eq!(fish_candidate.description.as_deref(), Some("Fish only"));
    }

    #[tokio::test]
    async fn dynamic_providers_complete_script_backed_command_values() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();

        write_executable_script(
            &bin_dir.join("cargo"),
            r#"#!/bin/sh
if [ "$1" = "metadata" ]; then
cat <<'JSON'
{"packages":[{"name":"app-core","targets":[{"name":"cli-tool","kind":["bin"]},{"name":"demo-example","kind":["example"]}]}]}
JSON
fi
"#,
        );
        write_executable_script(
            &bin_dir.join("git"),
            r#"#!/bin/sh
args="$*"
if [ "$1" = "for-each-ref" ]; then
  case "$args" in
    *"refs/heads"*"refs/remotes"*) printf 'main\norigin/release\norigin/HEAD\n' ;;
    *"refs/remotes"*) printf 'origin/main\norigin/release\nupstream/dev\norigin/HEAD\n' ;;
    *"refs/heads"*"refs/tags"*) printf 'main\nv1.0.0\n' ;;
    *"refs/heads"*) printf 'main\nfeature/demo\n' ;;
  esac
elif [ "$1" = "remote" ]; then
  printf 'origin\nupstream\n'
elif [ "$1" = "tag" ]; then
  printf 'v1.0.0\n'
elif [ "$1" = "stash" ] && [ "$2" = "list" ]; then
  printf 'stash@{0}: WIP on main\nstash@{1}: On dev\n'
elif [ "$1" = "status" ]; then
  printf ' M src/lib.rs\0?? README.md\0'
elif [ "$1" = "worktree" ] && [ "$2" = "list" ]; then
  printf 'worktree /tmp/demo-worktree\nHEAD abc123\n'
fi
"#,
        );
        write_executable_script(
            &bin_dir.join("systemctl"),
            "#!/bin/sh\ncase \"$1\" in\nlist-unit-files) printf 'ssh.service enabled\\n';;\nlist-units) printf 'docker.service loaded active running Docker\\n';;\nesac\n",
        );
        write_executable_script(
            &bin_dir.join("tmux"),
            "#!/bin/sh\nif [ \"$1\" = \"list-sessions\" ]; then printf 'dev-session\\nprod-session\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("gh"),
            "#!/bin/sh\nif [ \"$1\" = \"pr\" ] && [ \"$2\" = \"list\" ]; then printf '123\\n124\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("pip"),
            "#!/bin/sh\nif [ \"$1\" = \"list\" ]; then printf 'requests==2.0.0\\npytest==8.0.0\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("rustup"),
            "#!/bin/sh\nif [ \"$1\" = \"toolchain\" ] && [ \"$2\" = \"list\" ]; then printf 'stable-x86_64-unknown-linux-gnu (default)\\nnightly-x86_64-unknown-linux-gnu\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("nmcli"),
            "#!/bin/sh\nif [ \"$4\" = \"connection\" ]; then printf 'home-wifi\\nwork-vpn\\n'; elif [ \"$4\" = \"device\" ] && [ \"$5\" = \"status\" ]; then printf 'wlan0:connected\\neth0:disconnected\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("pacman"),
            "#!/bin/sh\ncase \"$1\" in\n-Qq) printf 'pacman\\nparu\\n';;\n-Slq) printf 'ripgrep\\nrust\\n';;\nesac\n",
        );
        write_executable_script(
            &bin_dir.join("docker"),
            "#!/bin/sh\nif [ \"$1\" = \"ps\" ]; then printf 'app-container\\nworker-container\\n'; elif [ \"$1\" = \"images\" ]; then printf 'app-image:latest\\nbase-image:latest\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("kubectl"),
            "#!/bin/sh\nif [ \"$1\" = \"get\" ] && [ \"$2\" = \"pods\" ]; then printf 'web-0\\napi-0\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("lsblk"),
            "#!/bin/sh\nif [ \"$1\" = \"-rno\" ]; then printf 'sda disk\\nsda1 part\\nloop0 loop\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("blkid"),
            "#!/bin/sh\nif [ \"$1\" = \"-o\" ]; then printf 'DEVNAME=/dev/sda1\\nUUID=abcd-1234\\nLABEL=rootfs\\n\\nDEVNAME=/dev/sdb1\\nUUID=beef-9999\\nLABEL=data\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("busctl"),
            "#!/bin/sh\nif [ \"$1\" = \"list\" ]; then printf 'org.freedesktop.login1 1 systemd root - - - Login\\norg.example.Demo 2 demo user - - - Demo\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("dpkg-query"),
            "#!/bin/sh\nif [ \"$1\" = \"-W\" ]; then printf 'base-files\\nbash\\ncoreutils\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("localectl"),
            "#!/bin/sh\ncase \"$1\" in\nlist-keymaps) printf 'jp106\\nus\\n';;\nlist-locales) printf 'en_US.UTF-8\\nja_JP.UTF-8\\n';;\nesac\n",
        );
        write_executable_script(
            &bin_dir.join("loginctl"),
            "#!/bin/sh\ncase \"$1\" in\nlist-sessions) printf '2 1000 alice seat0 tty2\\n';;\nlist-seats) printf 'seat0\\n';;\nesac\n",
        );
        write_executable_script(
            &bin_dir.join("losetup"),
            "#!/bin/sh\nif [ \"$1\" = \"--list\" ]; then printf '/dev/loop0\\n/dev/loop1\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("rpm"),
            "#!/bin/sh\nif [ \"$1\" = \"-qa\" ]; then printf 'kernel-core\\nbash\\nsystemd\\n'; fi\n",
        );
        write_executable_script(
            &bin_dir.join("timedatectl"),
            "#!/bin/sh\nif [ \"$1\" = \"list-timezones\" ]; then printf 'Asia/Tokyo\\nEurope/London\\n'; fi\n",
        );

        let engine = engine_with_path(&bin_dir);

        let cases = [
            ("cargo build -p ap", "app-core"),
            ("cargo run --bin cl", "cli-tool"),
            ("git switch fe", "feature/demo"),
            ("git checkout rel", "release"),
            ("git push origin fe", "feature/demo"),
            ("git pull origin ma", "main"),
            ("git tag v1", "v1.0.0"),
            ("git stash pop stash", "stash@{0}"),
            ("git add RE", "README.md"),
            ("git worktree remove demo", "/tmp/demo-worktree"),
            ("systemctl start ss", "ssh.service"),
            ("journalctl -u do", "docker.service"),
            ("tmux attach -t de", "dev-session"),
            ("gh pr view 12", "123"),
            ("pip show req", "requests"),
            ("rustup default sta", "stable-x86_64-unknown-linux-gnu"),
            ("nmcli connection up ho", "home-wifi"),
            ("nmcli device disconnect wl", "wlan0"),
            ("pacman -R pa", "pacman"),
            ("docker stop app", "app-container"),
            ("docker inspect app-i", "app-image:latest"),
            ("kubectl get pods we", "web-0"),
            ("fdisk /dev/s", "/dev/sda"),
            ("mount /dev/lo", "/dev/loop0"),
            ("blkid -U ab", "abcd-1234"),
            ("blkid -L root", "rootfs"),
            ("busctl introspect org.", "org.freedesktop.login1"),
            ("localectl set-keymap jp", "jp106"),
            ("localectl set-locale en", "en_US.UTF-8"),
            ("loginctl session-status 2", "2"),
            ("loginctl seat-status seat", "seat0"),
            ("losetup -d /dev/loop", "/dev/loop0"),
            ("timedatectl set-timezone Asia/T", "Asia/Tokyo"),
            ("apt remove bas", "base-files"),
            ("dnf remove ker", "kernel-core"),
            ("yum remove sys", "systemd"),
        ];

        for (input, expected) in cases {
            let _ = engine
                .complete(input, input.len(), dir.path(), 50, None)
                .await;
            let _ = wait_for_candidate(&engine, input, dir.path(), expected).await;
        }
    }

    #[tokio::test]
    async fn sysctl_key_completion_uses_proc_sys_keys_without_value_side() {
        if !Path::new("/proc/sys/net/ipv4/ip_forward").exists() {
            return;
        }

        let dir = tempdir().unwrap();
        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "sysctl net.ipv4.ip_for";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "net.ipv4.ip_forward"),
            "expected sysctl key completion in {:?}",
            result.candidates
        );

        let value_input = "sysctl net.ipv4.ip_forward=1";
        let value_result = engine
            .complete(value_input, value_input.len(), dir.path(), 50, None)
            .await;
        assert!(
            !value_result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "net.ipv4.ip_forward"),
            "sysctl key provider must not complete the value side"
        );
    }

    #[tokio::test]
    async fn js_dependency_completion_uses_package_json_without_script() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"react":"latest"},"devDependencies":{"vite":"latest"}}"#,
        )
        .unwrap();

        let mut engine = IntegratedCompletionEngine::new(Environment::new());
        engine.initialize_command_completion().unwrap();

        let input = "npm uninstall rea";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "react"),
            "expected package.json dependency completion in {:?}",
            result.candidates
        );
    }

    #[tokio::test]
    async fn ghost_completion_uses_cached_dynamic_values_only() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let counter = dir.path().join("tmux-count");
        write_executable_script(
            &bin_dir.join("tmux"),
            &format!(
                "#!/bin/sh\ncount_file=\"{}\"\ncount=0\nif [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$count_file\"\nif [ \"$1\" = \"list-sessions\" ]; then printf 'dev-session\\n'; fi\n",
                counter.display()
            ),
        );

        let engine = engine_with_path(&bin_dir);
        let input = "tmux attach -t de";

        assert_eq!(
            engine.ghost_completion(input, input.len(), dir.path(), None),
            None
        );
        assert!(
            !counter.exists(),
            "ghost completion must not run uncached dynamic commands"
        );

        let _ = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;
        let _ = wait_for_candidate(&engine, input, dir.path(), "dev-session").await;

        assert_eq!(
            engine.ghost_completion(input, input.len(), dir.path(), None),
            Some("tmux attach -t dev-session".to_string())
        );
    }

    #[tokio::test]
    async fn dev_dynamic_providers_complete_local_project_values() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module example.com/demo\n").unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\ndependencies = [\"requests>=2\", \"pytest\"]\n",
        )
        .unwrap();
        fs::write(dir.path().join("requirements-dev.txt"), "ruff==0.8\n").unwrap();
        let nested_go_dir = dir.path().join("pkg");
        fs::create_dir_all(&nested_go_dir).unwrap();

        let node_bin = dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&node_bin).unwrap();
        fs::write(node_bin.join("vite"), "").unwrap();
        fs::write(node_bin.join("eslint"), "").unwrap();

        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        write_executable_script(
            &bin_dir.join("go"),
            "#!/bin/sh\nif [ \"$1\" = list ]; then printf 'example.com/demo\\t%s\\n' \"$PWD\"; printf 'example.com/demo/pkg/api\\t%s/pkg/api\\n' \"$PWD\"; fi\n",
        );

        let engine = engine_with_path(&bin_dir);
        let cases = [
            ("uv remove req", "requests"),
            ("poetry remove py", "pytest"),
            ("npx vi", "vite"),
            ("npm exec es", "eslint"),
            ("go test ./p", "./pkg/api"),
        ];

        for (input, expected) in cases {
            let _ = engine
                .complete(input, input.len(), dir.path(), 50, None)
                .await;
            let _ = wait_for_candidate(&engine, input, dir.path(), expected).await;
        }

        let input = "go test ./p";
        let _ = engine
            .complete(input, input.len(), &nested_go_dir, 50, None)
            .await;
        let _ = wait_for_candidate(&engine, input, &nested_go_dir, "./pkg/api").await;
    }

    #[tokio::test]
    async fn python_module_dynamic_providers_complete_local_modules() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\ndependencies = [\"fastapi>=0.110\"]\n",
        )
        .unwrap();
        let package_dir = dir.path().join("src").join("demo_app");
        fs::create_dir_all(&package_dir).unwrap();
        fs::write(package_dir.join("__init__.py"), "").unwrap();
        fs::write(package_dir.join("cli.py"), "").unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        for (input, expected) in [
            ("python -m dem", "demo_app"),
            ("python3 -m demo_app.c", "demo_app.cli"),
            ("pytest --cov dem", "demo_app"),
            ("mypy -m demo_app.c", "demo_app.cli"),
            ("mypy -p fast", "fastapi"),
        ] {
            let result = wait_for_candidate(&engine, input, dir.path(), expected).await;
            assert!(
                result
                    .candidates
                    .iter()
                    .all(|candidate| candidate.candidate_type == CandidateType::Argument),
                "{input} should return module argument candidates: {:?}",
                result.candidates
            );
        }
    }

    #[tokio::test]
    async fn node_workspace_and_bin_completion_walks_monorepo_ancestors() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{ "private": true, "workspaces": ["packages/*"] }"#,
        )
        .unwrap();
        let package_dir = dir.path().join("packages").join("web");
        fs::create_dir_all(&package_dir).unwrap();
        fs::write(
            package_dir.join("package.json"),
            r#"{ "name": "@demo/web", "scripts": { "build": "vite build" } }"#,
        )
        .unwrap();
        let node_bin = dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&node_bin).unwrap();
        fs::write(node_bin.join("vite"), "").unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        for (input, expected) in [
            ("npx vi", "vite"),
            ("npm exec vi", "vite"),
            ("pnpm exec vi", "vite"),
            ("yarn exec vi", "vite"),
            ("bun x vi", "vite"),
            ("npm --workspace @demo", "@demo/web"),
            ("pnpm --filter @demo", "@demo/web"),
            ("yarn workspace @demo", "@demo/web"),
            ("turbo run build --filter @demo", "@demo/web"),
        ] {
            let _ = engine
                .complete(input, input.len(), &package_dir, 50, None)
                .await;
            let _ = wait_for_candidate(&engine, input, &package_dir, expected).await;
        }
    }

    #[tokio::test]
    async fn cloud_and_terraform_dynamic_providers_read_local_fixtures() {
        let dir = tempdir().unwrap();
        let aws_dir = dir.path().join(".aws");
        fs::create_dir_all(&aws_dir).unwrap();
        let aws_config = aws_dir.join("config");
        let aws_credentials = aws_dir.join("credentials");
        fs::write(
            &aws_config,
            "[default]\nregion = us-east-1\n[profile dev]\nregion = us-west-2\n",
        )
        .unwrap();
        fs::write(&aws_credentials, "[prod]\naws_access_key_id = test\n").unwrap();

        let gcloud_dir = dir.path().join("gcloud");
        let gcloud_configurations = gcloud_dir.join("configurations");
        fs::create_dir_all(&gcloud_configurations).unwrap();
        fs::write(
            gcloud_configurations.join("config_dev"),
            "project = demo-dev\n",
        )
        .unwrap();
        fs::write(
            gcloud_configurations.join("config_prod"),
            "project = demo-prod\n",
        )
        .unwrap();

        let terraform_dir = dir.path().join(".terraform");
        fs::create_dir_all(terraform_dir.join("terraform.tfstate.d").join("dev")).unwrap();
        fs::write(terraform_dir.join("environment"), "staging\n").unwrap();

        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.set_system_env_var("HOME".to_string(), dir.path().display().to_string());
            env.set_system_env_var(
                "AWS_CONFIG_FILE".to_string(),
                aws_config.display().to_string(),
            );
            env.set_system_env_var(
                "AWS_SHARED_CREDENTIALS_FILE".to_string(),
                aws_credentials.display().to_string(),
            );
            env.set_system_env_var(
                "CLOUDSDK_CONFIG".to_string(),
                gcloud_dir.display().to_string(),
            );
        }
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        for (input, expected) in [
            ("aws --profile de", "dev"),
            ("gcloud --configuration de", "dev"),
            ("gcloud --project demo-p", "demo-prod"),
            ("terraform workspace select sta", "staging"),
            ("tofu workspace delete de", "dev"),
        ] {
            let result = wait_for_candidate(&engine, input, dir.path(), expected).await;
            assert!(
                result
                    .candidates
                    .iter()
                    .all(|candidate| candidate.candidate_type == CandidateType::Argument),
                "{input} should return local config argument candidates: {:?}",
                result.candidates
            );
        }
    }

    #[tokio::test]
    async fn container_dynamic_providers_complete_fake_cli_objects() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let script = r#"#!/bin/sh
case "$1" in
  images) printf 'localhost/app:latest\n<none>:<none>\n' ;;
  ps)
    if [ "$2" = "-a" ]; then
      printf 'web\nold\n'
    else
      printf 'web\n'
    fi
    ;;
  network)
    if [ "$2" = "ls" ]; then printf 'frontend\nbackend\n'; fi
    ;;
  volume)
    if [ "$2" = "ls" ]; then printf 'cache\nlogs\n'; fi
    ;;
esac
"#;
        write_executable_script(&bin_dir.join("docker"), script);
        write_executable_script(&bin_dir.join("podman"), script);

        let engine = engine_with_path(&bin_dir);
        for (input, expected) in [
            ("docker run loc", "localhost/app:latest"),
            ("docker rm ol", "old"),
            ("docker network rm fr", "frontend"),
            ("docker volume rm ca", "cache"),
            ("podman run loc", "localhost/app:latest"),
            ("podman stop we", "web"),
            ("podman network inspect back", "backend"),
            ("podman volume inspect lo", "logs"),
        ] {
            let _ = engine
                .complete(input, input.len(), dir.path(), 50, None)
                .await;
            let _ = wait_for_candidate(&engine, input, dir.path(), expected).await;
        }
    }

    #[tokio::test]
    async fn nx_run_completion_reads_workspace_and_descendant_projects() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("workspace.json"),
            r#"{
              "projects": {
                "web": { "targets": { "build": {}, "test": {} } },
                "legacy": { "architect": { "serve": {} } }
              }
            }"#,
        )
        .unwrap();
        let api_dir = dir.path().join("apps").join("api");
        fs::create_dir_all(&api_dir).unwrap();
        fs::write(
            api_dir.join("project.json"),
            r#"{ "name": "api", "targets": { "lint": {} } }"#,
        )
        .unwrap();
        let tasks = dsh_builtin::task::list_tasks_in_dir_for_sources(dir.path(), &["nx"]).unwrap();
        assert!(
            tasks.iter().any(|task| task.command == "nx run web:build"),
            "expected Nx task detection to include web:build: {tasks:?}"
        );

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        for (input, expected) in [
            ("nx run web:b", "web:build"),
            ("nx run legacy:s", "legacy:serve"),
            ("nx run api:l", "api:lint"),
        ] {
            let result = wait_for_candidate(&engine, input, dir.path(), expected).await;
            assert!(
                !result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == "build"),
                "nx run should use project:target candidates, not bare targets: {:?}",
                result.candidates
            );
        }
    }

    #[tokio::test]
    async fn project_task_scope_limits_dev_task_sources() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "build-npm": "echo npm" } }"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("deno.json"),
            r#"{ "tasks": { "build-deno": "deno run main.ts" } }"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("turbo.json"),
            r#"{ "tasks": { "build-turbo": {} } }"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("project.json"),
            r#"{ "name": "app", "targets": { "build-nx": {} } }"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("mise.toml"),
            "[tasks.build-mise]\nrun = 'echo mise'\n",
        )
        .unwrap();

        let environment = Environment::new();
        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        for (input, expected) in [
            ("deno task bu", "build-deno"),
            ("turbo run bu", "build-turbo"),
            ("nx run app:b", "app:build-nx"),
            ("mise run bu", "build-mise"),
        ] {
            let result = wait_for_candidate(&engine, input, dir.path(), expected).await;
            assert!(
                !result
                    .candidates
                    .iter()
                    .any(|candidate| candidate.text == "build-npm"),
                "{input} should not include npm tasks from another project.task scope: {:?}",
                result.candidates
            );
        }
    }
}
