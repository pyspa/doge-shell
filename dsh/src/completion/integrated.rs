use super::cache::CompletionCache;
use super::command::{CommandCompletionDatabase, CompletionCandidate};

use super::framework::CompletionFrameworkKind;

use super::generator::CompletionGenerator;
use crate::completion::generators::filesystem::FileSystemGenerator;

use super::json_loader::JsonCompletionLoader;
use super::parser::{self, CommandLineParser, ParsedCommandLine};
use crate::completion::display::Candidate;
use crate::environment::Environment;
use anyhow::Result;
use dsh_builtin::{project, project_context, task};
use dsh_types::mcp::McpTransport;
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, warn};
use wait_timeout::ChildExt;

const DEFAULT_CACHE_TTL_MS: u64 = 3000;
const DYNAMIC_COMMAND_CACHE_TTL_MS: u64 = 1000;

#[derive(Debug, Clone, Copy)]
struct CompletionRequest<'a> {
    input: &'a str,
    #[allow(dead_code)]
    current_dir: &'a Path,
    max_results: usize,
    #[allow(dead_code)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileMetadataSignature {
    exists: bool,
    modified: Option<SystemTime>,
    len: u64,
}

#[derive(Debug, Clone)]
struct TaskCacheEntry {
    signature: Vec<FileMetadataSignature>,
    tasks: Vec<task::TaskInfo>,
}

#[derive(Debug, Clone)]
struct ComposeCacheEntry {
    signature: FileMetadataSignature,
    services: Vec<String>,
}

#[derive(Debug, Clone)]
struct CommandValueCacheEntry {
    values: Vec<String>,
    cached_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DynamicCommandCacheKind {
    GitBranch,
    GitRemote,
    GitWorktree,
    KubectlContext,
    KubectlNamespace,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DynamicCommandCacheKey {
    kind: DynamicCommandCacheKind,
    scope_dir: PathBuf,
}

#[derive(Debug, Default)]
struct ProjectDynamicCache {
    tasks: HashMap<PathBuf, TaskCacheEntry>,
    compose_services: HashMap<PathBuf, ComposeCacheEntry>,
    commands: HashMap<DynamicCommandCacheKey, CommandValueCacheEntry>,
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
    dynamic_cache: RwLock<ProjectDynamicCache>,

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
            dynamic_cache: RwLock::new(ProjectDynamicCache::default()),
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
        history: Option<&Arc<parking_lot::Mutex<crate::history::History>>>,
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

        let parsed_command_line = self.convert_to_parsed_command_line(input, cursor_pos);
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
            let results = aggregator.finalize(history);
            self.store_in_cache(request.input, &results.candidates, results.framework);
            return results;
        }

        // 2. JSON-based command completion
        let command_collection = self.collect_command_candidates(&request, &parsed_command_line);
        if !aggregator.extend(command_collection.batch) {
            let results = aggregator.finalize(history);
            self.store_in_cache(request.input, &results.candidates, results.framework);
            return results;
        }

        // 3. External completer fallback
        let external_batch = self.collect_external_candidates(&request, &parsed_command_line);
        if !aggregator.extend(external_batch) {
            let results = aggregator.finalize(history);
            self.store_in_cache(request.input, &results.candidates, results.framework);
            return results;
        }

        // 4. Context analysis placeholder (reserved for future providers)
        let parts: Vec<&str> = request.input.split_whitespace().collect();
        if !parts.is_empty() {
            let _command = parts[0];
            let _args: Vec<String> = parts[1..].iter().map(|s| (*s).to_string()).collect();
        }

        // 5. History-based completion (skipped when command-specific data exists)
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
            "task" => self.collect_task_candidates(parsed_command_line, request.current_dir),
            "pm" => self.collect_pm_candidates(parsed_command_line),
            "pj" => self.collect_pj_candidates(parsed_command_line),
            "mcp" => self.collect_mcp_candidates(parsed_command_line),
            "git" => self.collect_git_candidates(parsed_command_line, request.current_dir),
            "docker" => self.collect_docker_candidates(parsed_command_line, request.current_dir),
            "kubectl" => self.collect_kubectl_candidates(parsed_command_line, request.current_dir),
            _ => Vec::new(),
        };

        if candidates.is_empty() {
            CandidateBatch::empty()
        } else {
            CandidateBatch::inclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
        }
    }

    fn collect_task_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        let current_token = parsed_command_line.current_token.as_str();
        match self.load_project_tasks(current_dir) {
            Ok(tasks) => tasks
                .into_iter()
                .filter(|task| matches_prefix(current_token, &task.name))
                .map(|task| EnhancedCandidate {
                    text: task.name,
                    description: Some(format_task_description(&task.source, &task.command)),
                    candidate_type: CandidateType::Argument,
                    priority: 90,
                })
                .collect(),
            Err(e) => {
                warn!("Failed to load task completions: {}", e);
                Vec::new()
            }
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

    fn collect_git_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        use parser::CompletionContext;

        let Some(primary_subcommand) = parsed_command_line.subcommand_path.first() else {
            return Vec::new();
        };
        let current_token = parsed_command_line.current_token.as_str();
        let inferred_subcommand_arg_index =
            parsed_command_line.subcommand_path.len().saturating_sub(2);

        match parsed_command_line.completion_context {
            CompletionContext::Argument { arg_index, .. } => match primary_subcommand.as_str() {
                "checkout" | "switch" | "merge" | "rebase" => {
                    self.collect_git_branch_candidates(current_dir, current_token)
                }
                "push" | "pull" | "fetch" => {
                    if arg_index == 0 {
                        self.collect_git_remote_candidates(current_dir, current_token)
                    } else {
                        self.collect_git_branch_candidates(current_dir, current_token)
                    }
                }
                "remote" => {
                    let secondary = parsed_command_line
                        .subcommand_path
                        .get(1)
                        .map(String::as_str)
                        .unwrap_or("");
                    match secondary {
                        "remove" | "rename" | "show" | "get-url" | "set-url" => {
                            self.collect_git_remote_candidates(current_dir, current_token)
                        }
                        _ => Vec::new(),
                    }
                }
                "worktree" => {
                    let secondary = parsed_command_line
                        .subcommand_path
                        .get(1)
                        .map(String::as_str)
                        .unwrap_or("");
                    match secondary {
                        "remove" | "move" | "lock" | "unlock" | "repair" => {
                            self.collect_git_worktree_candidates(current_dir, current_token)
                        }
                        "add" if arg_index > 0 => {
                            self.collect_git_branch_candidates(current_dir, current_token)
                        }
                        _ => Vec::new(),
                    }
                }
                _ => Vec::new(),
            },
            CompletionContext::SubCommand => match primary_subcommand.as_str() {
                "checkout" | "switch" | "merge" | "rebase" => {
                    self.collect_git_branch_candidates(current_dir, current_token)
                }
                "push" | "pull" | "fetch" => {
                    if inferred_subcommand_arg_index == 0 {
                        self.collect_git_remote_candidates(current_dir, current_token)
                    } else {
                        self.collect_git_branch_candidates(current_dir, current_token)
                    }
                }
                "remote" => {
                    let secondary = parsed_command_line
                        .subcommand_path
                        .get(1)
                        .map(String::as_str)
                        .unwrap_or("");
                    match secondary {
                        "remove" | "rename" | "show" | "get-url" | "set-url" => {
                            self.collect_git_remote_candidates(current_dir, current_token)
                        }
                        _ => Vec::new(),
                    }
                }
                "worktree" => {
                    let secondary = parsed_command_line
                        .subcommand_path
                        .get(1)
                        .map(String::as_str)
                        .unwrap_or("");
                    match secondary {
                        "remove" | "move" | "lock" | "unlock" | "repair" => {
                            self.collect_git_worktree_candidates(current_dir, current_token)
                        }
                        "add" if inferred_subcommand_arg_index > 0 => {
                            self.collect_git_branch_candidates(current_dir, current_token)
                        }
                        _ => Vec::new(),
                    }
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn collect_docker_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        use parser::CompletionContext;

        if parsed_command_line
            .subcommand_path
            .first()
            .map(String::as_str)
            != Some("compose")
        {
            return Vec::new();
        }

        let Some(command_name) = parsed_command_line
            .subcommand_path
            .get(1)
            .map(String::as_str)
        else {
            return Vec::new();
        };

        match parsed_command_line.completion_context {
            CompletionContext::SubCommand | CompletionContext::Argument { .. } => {
                let service_commands = [
                    "build", "cp", "create", "down", "exec", "kill", "logs", "pause", "port", "ps",
                    "pull", "push", "restart", "rm", "run", "scale", "start", "stop", "top",
                    "unpause", "up", "wait",
                ];

                if service_commands.contains(&command_name) {
                    let current_token = parsed_command_line.current_token.as_str();
                    self.collect_compose_service_candidates(current_dir, current_token)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn collect_kubectl_candidates(
        &self,
        parsed_command_line: &ParsedCommandLine,
        current_dir: &Path,
    ) -> Vec<EnhancedCandidate> {
        use parser::CompletionContext;

        let current_token = parsed_command_line.current_token.as_str();
        match &parsed_command_line.completion_context {
            CompletionContext::OptionValue { option_name, .. } => match option_name.as_str() {
                "--context" => self.collect_kubectl_context_candidates(current_dir, current_token),
                "-n" | "--namespace" => {
                    self.collect_kubectl_namespace_candidates(current_dir, current_token)
                }
                _ => Vec::new(),
            },
            CompletionContext::SubCommand | CompletionContext::Argument { .. } => {
                let path = parsed_command_line
                    .subcommand_path
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                if path.len() >= 2 && path[0] == "config" && path[1] == "use-context" {
                    self.collect_kubectl_context_candidates(current_dir, current_token)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn collect_git_branch_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitBranch,
            scope_dir,
            current_token,
            "git branch",
            || {
                let Some(command_path) = self.resolve_command_path("git") else {
                    return Ok(Vec::new());
                };

                run_command_lines(
                    &command_path,
                    &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
                    current_dir,
                )
            },
        )
    }

    fn collect_git_remote_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitRemote,
            scope_dir,
            current_token,
            "git remote",
            || {
                let Some(command_path) = self.resolve_command_path("git") else {
                    return Ok(Vec::new());
                };

                run_command_lines(&command_path, &["remote"], current_dir)
            },
        )
    }

    fn collect_git_worktree_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        let scope_dir = project_context::find_project_root(current_dir);
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::GitWorktree,
            scope_dir,
            current_token,
            "git worktree",
            || {
                let Some(command_path) = self.resolve_command_path("git") else {
                    return Ok(Vec::new());
                };

                Ok(run_command_lines(
                    &command_path,
                    &["worktree", "list", "--porcelain"],
                    current_dir,
                )?
                .into_iter()
                .filter_map(|line| line.strip_prefix("worktree ").map(str::to_string))
                .collect())
            },
        )
    }

    fn collect_compose_service_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        match self.load_compose_services(current_dir) {
            Ok(Some((compose_file, services))) => services
                .into_iter()
                .filter(|service| matches_prefix(current_token, service))
                .map(|service| EnhancedCandidate {
                    text: service,
                    description: Some(format!("compose service ({})", compose_file.display())),
                    candidate_type: CandidateType::Argument,
                    priority: 125,
                })
                .collect(),
            Ok(None) => Vec::new(),
            Err(err) => {
                warn!(
                    "Failed to load compose services from {:?}: {}",
                    current_dir, err
                );
                Vec::new()
            }
        }
    }

    fn collect_kubectl_context_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::KubectlContext,
            canonicalize_path(current_dir),
            current_token,
            "kubectl context",
            || {
                let Some(command_path) = self.resolve_command_path("kubectl") else {
                    return Ok(Vec::new());
                };

                run_command_lines(
                    &command_path,
                    &["config", "get-contexts", "-o", "name"],
                    current_dir,
                )
            },
        )
    }

    fn collect_kubectl_namespace_candidates(
        &self,
        current_dir: &Path,
        current_token: &str,
    ) -> Vec<EnhancedCandidate> {
        self.collect_cached_command_candidates(
            DynamicCommandCacheKind::KubectlNamespace,
            canonicalize_path(current_dir),
            current_token,
            "kubectl namespace",
            || {
                let Some(command_path) = self.resolve_command_path("kubectl") else {
                    return Ok(Vec::new());
                };

                run_command_lines(
                    &command_path,
                    &[
                        "get",
                        "namespaces",
                        "-o",
                        "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}",
                    ],
                    current_dir,
                )
            },
        )
    }

    fn collect_cached_command_candidates<F>(
        &self,
        kind: DynamicCommandCacheKind,
        scope_dir: PathBuf,
        current_token: &str,
        description: &str,
        loader: F,
    ) -> Vec<EnhancedCandidate>
    where
        F: FnOnce() -> Result<Vec<String>>,
    {
        match self.load_command_values(kind, scope_dir, loader) {
            Ok(values) => values
                .into_iter()
                .filter(|value| matches_prefix(current_token, value))
                .map(|value| EnhancedCandidate {
                    text: value,
                    description: Some(description.to_string()),
                    candidate_type: CandidateType::Argument,
                    priority: 130,
                })
                .collect(),
            Err(err) => {
                warn!("Failed to load {} completions: {}", description, err);
                Vec::new()
            }
        }
    }

    fn load_project_tasks(&self, current_dir: &Path) -> Result<Vec<task::TaskInfo>> {
        let project_root = project_context::find_project_root(current_dir);
        let signature = task_completion_signature(&project_root);

        if let Some(tasks) = self.lookup_task_cache(&project_root, &signature) {
            return Ok(tasks);
        }

        let tasks = task::list_tasks_in_dir(&project_root)?;
        self.dynamic_cache.write().tasks.insert(
            project_root,
            TaskCacheEntry {
                signature,
                tasks: tasks.clone(),
            },
        );
        Ok(tasks)
    }

    fn lookup_task_cache(
        &self,
        project_root: &Path,
        signature: &[FileMetadataSignature],
    ) -> Option<Vec<task::TaskInfo>> {
        let cache = self.dynamic_cache.read();
        let entry = cache.tasks.get(project_root)?;
        if entry.signature == signature {
            Some(entry.tasks.clone())
        } else {
            None
        }
    }

    fn load_compose_services(&self, current_dir: &Path) -> Result<Option<(PathBuf, Vec<String>)>> {
        let Some(compose_file) = find_compose_file(current_dir) else {
            return Ok(None);
        };
        let cache_key = canonicalize_path(&compose_file);
        let signature = file_metadata_signature(&cache_key);

        if let Some(services) = self.lookup_compose_cache(&cache_key, &signature) {
            return Ok(Some((cache_key, services)));
        }

        let services = parse_compose_service_names(&cache_key)?;
        self.dynamic_cache.write().compose_services.insert(
            cache_key.clone(),
            ComposeCacheEntry {
                signature,
                services: services.clone(),
            },
        );

        Ok(Some((cache_key, services)))
    }

    fn lookup_compose_cache(
        &self,
        compose_file: &Path,
        signature: &FileMetadataSignature,
    ) -> Option<Vec<String>> {
        let cache = self.dynamic_cache.read();
        let entry = cache.compose_services.get(compose_file)?;
        if entry.signature == *signature {
            Some(entry.services.clone())
        } else {
            None
        }
    }

    fn load_command_values<F>(
        &self,
        kind: DynamicCommandCacheKind,
        scope_dir: PathBuf,
        loader: F,
    ) -> Result<Vec<String>>
    where
        F: FnOnce() -> Result<Vec<String>>,
    {
        let cache_key = DynamicCommandCacheKey { kind, scope_dir };
        let ttl = Duration::from_millis(DYNAMIC_COMMAND_CACHE_TTL_MS);

        {
            let cache = self.dynamic_cache.read();
            if let Some(entry) = cache.commands.get(&cache_key)
                && entry.cached_at.elapsed() < ttl
            {
                return Ok(entry.values.clone());
            }
        }

        let values = loader()?;
        self.dynamic_cache.write().commands.insert(
            cache_key,
            CommandValueCacheEntry {
                values: values.clone(),
                cached_at: Instant::now(),
            },
        );
        Ok(values)
    }

    fn resolve_command_path(&self, command_name: &str) -> Option<String> {
        self.environment.read().lookup(command_name)
    }

    fn collect_external_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &ParsedCommandLine,
    ) -> CandidateBatch {
        let Some(command_template) = self
            .environment
            .read()
            .get_var("DSH_EXTERNAL_COMPLETER")
            .filter(|value| !value.trim().is_empty())
        else {
            return CandidateBatch::empty();
        };

        match run_external_completer(
            &command_template,
            request.current_dir,
            request.input,
            request.cursor_pos,
            parsed_command_line,
        ) {
            Ok(candidates) if !candidates.is_empty() => {
                CandidateBatch::inclusive_with_framework(candidates, CompletionFrameworkKind::Skim)
            }
            Ok(_) => CandidateBatch::empty(),
            Err(err) => {
                warn!("External completer failed: {}", err);
                CandidateBatch::empty()
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
        if let Some(history_arc) = history {
            let history = history_arc.lock();
            for candidate in &mut candidates {
                if matches!(
                    candidate.candidate_type,
                    CandidateType::File | CandidateType::Directory
                ) {
                    continue;
                }

                // Simplified boosting for linear history
                let mut score = 0;
                for item in history.iter().rev().take(2000) {
                    // Check recent 2000 explicitly
                    if item.entry.contains(&candidate.text) {
                        let mut item_score = 10;
                        // Context-aware boosting
                        if let Some(cmd) = command_context
                            && (item.entry == *cmd
                                || item.entry.starts_with(&(cmd.to_string() + " ")))
                        {
                            item_score += 500;
                        }
                        score += item_score;
                        if score > 5000 {
                            break;
                        }
                    }
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

fn matches_prefix(current_token: &str, value: &str) -> bool {
    current_token.is_empty() || super::fuzzy_match_score(value, current_token).is_some()
}

fn canonicalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn file_metadata_signature(path: &Path) -> FileMetadataSignature {
    match fs::metadata(path) {
        Ok(metadata) => FileMetadataSignature {
            exists: true,
            modified: metadata.modified().ok(),
            len: metadata.len(),
        },
        Err(_) => FileMetadataSignature {
            exists: false,
            modified: None,
            len: 0,
        },
    }
}

fn task_completion_signature(project_root: &Path) -> Vec<FileMetadataSignature> {
    [
        "mise.toml",
        "Taskfile.yml",
        "Taskfile.yaml",
        "turbo.json",
        "project.json",
        "package.json",
        "Cargo.toml",
        "Makefile",
        "makefile",
        "deno.json",
        "deno.jsonc",
    ]
    .into_iter()
    .map(|name| file_metadata_signature(&project_root.join(name)))
    .collect()
}

fn find_compose_file(current_dir: &Path) -> Option<PathBuf> {
    const CANDIDATES: [&str; 4] = [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    ];

    current_dir.ancestors().find_map(|dir| {
        CANDIDATES
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.exists())
    })
}

fn parse_compose_service_names(path: &Path) -> Result<Vec<String>> {
    let contents = fs::read_to_string(path)?;
    let mut in_services = false;
    let mut services_indent = 0usize;
    let mut service_indent = None;
    let mut names = Vec::new();

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.chars().take_while(|c| c.is_whitespace()).count();
        if !in_services {
            if trimmed == "services:" {
                in_services = true;
                services_indent = indent;
            }
            continue;
        }

        if indent <= services_indent {
            break;
        }

        if trimmed.starts_with('-') {
            continue;
        }

        if !trimmed.ends_with(':') {
            continue;
        }

        let key = trimmed.trim_end_matches(':').trim();
        if key.is_empty() || key.contains(' ') {
            continue;
        }

        match service_indent {
            None => {
                service_indent = Some(indent);
                names.push(key.to_string());
            }
            Some(expected_indent) if indent == expected_indent => names.push(key.to_string()),
            _ => {}
        }
    }

    let mut seen = HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    Ok(names)
}

fn run_external_completer(
    command_template: &str,
    current_dir: &Path,
    input: &str,
    cursor_pos: usize,
    parsed_command_line: &ParsedCommandLine,
) -> Result<Vec<EnhancedCandidate>> {
    let subcommand_path = parsed_command_line.subcommand_path.join(" ");
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command_template)
        .current_dir(current_dir)
        .env("DSH_COMPLETION_INPUT", input)
        .env("DSH_COMPLETION_CURSOR", cursor_pos.to_string())
        .env("DSH_COMPLETION_COMMAND", &parsed_command_line.command)
        .env(
            "DSH_COMPLETION_CURRENT_TOKEN",
            &parsed_command_line.current_token,
        )
        .env("DSH_COMPLETION_SUBCOMMAND_PATH", &subcommand_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let lines = wait_and_collect_lines(&mut child)?;
    Ok(lines
        .into_iter()
        .filter_map(|line| {
            let (text, description) = if let Some((text, description)) = line.split_once('\t') {
                (text.trim(), Some(description.trim().to_string()))
            } else {
                (line.trim(), None)
            };

            if text.is_empty() || !matches_prefix(&parsed_command_line.current_token, text) {
                return None;
            }

            Some(EnhancedCandidate {
                text: text.to_string(),
                description,
                candidate_type: CandidateType::Argument,
                priority: 200,
            })
        })
        .collect())
}

fn run_command_lines(command_path: &str, args: &[&str], current_dir: &Path) -> Result<Vec<String>> {
    let mut child = Command::new(command_path)
        .args(args)
        .current_dir(current_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    wait_and_collect_lines(&mut child)
}

fn wait_and_collect_lines(child: &mut std::process::Child) -> Result<Vec<String>> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Child stdout not captured"))?;
    let reader_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        stdout.read_to_string(&mut buf)?;
        Ok::<String, std::io::Error>(buf)
    });

    match child.wait_timeout(Duration::from_millis(1500))? {
        Some(status) => {
            let output = reader_thread
                .join()
                .map_err(|_| anyhow::anyhow!("Stdout reader thread panicked"))??;
            if !status.success() {
                return Ok(Vec::new());
            }

            Ok(output
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect())
        }
        None => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader_thread.join();
            Ok(Vec::new())
        }
    }
}

fn format_task_description(source: &str, command: &str) -> String {
    let summary = format!("{source}: {command}");
    truncate_string(&summary, 80)
}

fn truncate_string(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut out: String = value.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
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

    #[test]
    fn parse_compose_services_reads_top_level_service_names() {
        let dir = tempdir().unwrap();
        let compose_file = dir.path().join("compose.yaml");
        fs::write(
            &compose_file,
            r#"
services:
  api:
    image: example/api
  worker:
    build: .
volumes:
  cache:
"#,
        )
        .unwrap();

        let services = parse_compose_service_names(&compose_file).unwrap();
        assert_eq!(services, vec!["api".to_string(), "worker".to_string()]);
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
    }

    #[tokio::test]
    async fn external_completer_runs_as_fallback() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("external-completer.sh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'deploy\\tExternal completer\\n'\nprintf 'debug\\tExternal completer\\n'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let environment = Environment::new();
        environment.write().variables.insert(
            "DSH_EXTERNAL_COMPLETER".to_string(),
            script.display().to_string(),
        );

        let mut engine = IntegratedCompletionEngine::new(environment);
        engine.initialize_command_completion().unwrap();

        let input = "unknown-command de";
        let result = engine
            .complete(input, input.len(), dir.path(), 50, None)
            .await;

        assert!(
            result
                .candidates
                .iter()
                .any(|candidate| candidate.text == "deploy"),
            "expected external completer fallback"
        );
    }

    #[test]
    fn task_completion_cache_refreshes_when_taskfile_changes() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("apps").join("api");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n  build:\n    cmds:\n      - cargo build\n",
        )
        .unwrap();

        let engine = IntegratedCompletionEngine::new(Environment::new());
        let build_candidates = engine.collect_task_candidates(
            &engine.convert_to_parsed_command_line("task bu", "task bu".len()),
            &nested,
        );

        assert!(
            build_candidates
                .iter()
                .any(|candidate| candidate.text == "build"),
            "expected task completion from project root"
        );

        std::thread::sleep(Duration::from_millis(20));
        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n  build:\n    cmds:\n      - cargo build\n  test:\n    cmds:\n      - cargo test\n",
        )
        .unwrap();

        let test_candidates = engine.collect_task_candidates(
            &engine.convert_to_parsed_command_line("task te", "task te".len()),
            &nested,
        );

        assert!(
            test_candidates
                .iter()
                .any(|candidate| candidate.text == "test"),
            "expected task cache invalidation after Taskfile change"
        );
    }

    #[test]
    fn compose_service_cache_refreshes_when_compose_file_changes() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("services").join("api");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            dir.path().join("compose.yaml"),
            "services:\n  api:\n    image: example/api\n",
        )
        .unwrap();

        let engine = IntegratedCompletionEngine::new(Environment::new());
        let api_candidates = engine.collect_compose_service_candidates(&nested, "ap");

        assert!(
            api_candidates
                .iter()
                .any(|candidate| candidate.text == "api"),
            "expected compose service completion from ancestor compose file"
        );

        std::thread::sleep(Duration::from_millis(20));
        fs::write(
            dir.path().join("compose.yaml"),
            "services:\n  api:\n    image: example/api\n  worker:\n    image: example/worker\n",
        )
        .unwrap();

        let worker_candidates = engine.collect_compose_service_candidates(&nested, "wo");
        assert!(
            worker_candidates
                .iter()
                .any(|candidate| candidate.text == "worker"),
            "expected compose cache invalidation after file change"
        );
    }

    #[test]
    fn git_branch_cache_is_shared_per_project_root_and_expires_by_ttl() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("repo");
        let nested = root.join("apps").join("web");
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();
        fs::write(root.join("package.json"), "{\"name\":\"demo\"}\n").unwrap();

        let counter = dir.path().join("git-count");
        let git = bin_dir.join("git");
        fs::write(
            &git,
            format!(
                "#!/bin/sh\ncount_file=\"{}\"\ncount=0\nif [ -f \"$count_file\" ]; then\n  count=$(cat \"$count_file\")\nfi\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$count_file\"\nif [ \"$1\" = \"for-each-ref\" ]; then\n  printf 'feature/cache\\nmain\\n'\nfi\n",
                counter.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&git).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&git, permissions).unwrap();

        let environment = Environment::new();
        {
            let mut env = environment.write();
            env.paths = vec![bin_dir.display().to_string()];
            env.clear_command_cache();
        }
        let engine = IntegratedCompletionEngine::new(environment);

        let root_candidates = engine.collect_git_branch_candidates(&root, "fe");
        assert!(
            root_candidates
                .iter()
                .any(|candidate| candidate.text == "feature/cache"),
            "expected git branch completion from fake git"
        );

        let nested_candidates = engine.collect_git_branch_candidates(&nested, "ma");
        assert!(
            nested_candidates
                .iter()
                .any(|candidate| candidate.text == "main"),
            "expected cached git branch completion from nested cwd"
        );

        assert_eq!(
            fs::read_to_string(&counter).unwrap(),
            "1",
            "git command should run once within the same project root"
        );

        std::thread::sleep(Duration::from_millis(DYNAMIC_COMMAND_CACHE_TTL_MS + 50));
        let refreshed_candidates = engine.collect_git_branch_candidates(&nested, "fe");
        assert!(
            refreshed_candidates
                .iter()
                .any(|candidate| candidate.text == "feature/cache"),
            "expected git branch completion after ttl refresh"
        );

        assert_eq!(
            fs::read_to_string(&counter).unwrap(),
            "2",
            "git command should run again after ttl expiry"
        );
    }
}
