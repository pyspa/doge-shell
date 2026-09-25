//! Narrow capability boundaries for builtin implementations.
//!
//! [`crate::ShellProxy`] remains the public compatibility facade. Every proxy
//! automatically implements these traits, while new builtin helpers and tests
//! can depend on only the operations they actually need.

use crate::chatgpt::McpManager;
use crate::{CoreShellAction, ProxyFuture, ShellProxy};
use anyhow::Result;
use dsh_types::cron::job::{
    ClaimedRun, CronHealth, CronIncident, CronJobPatch, CronJobSpec, CronJobView, CronRun,
    IncidentKind, RunOutcome, RunOutput, RunQuery, RunSelector, RunTrigger,
};
use dsh_types::cron::tool::CronToolRequest;
use dsh_types::{
    Context, command_block::CommandBlock, mcp::McpServerConfig, output_history::OutputEntry,
    safety_policy::SafetyLevel, shell_options::ShellOption, snippet::Snippet,
};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Command execution, cancellation, confirmation, hooks, and interactive I/O.
pub trait ShellExecution {
    fn exit_shell(&mut self);
    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()>;
    fn dispatch_core_action(
        &mut self,
        ctx: &Context,
        action: CoreShellAction,
        argv: Vec<String>,
    ) -> Result<()>;
    fn request_eval_command(&mut self, command: String) -> Result<()>;
    fn capture_command(&mut self, ctx: &Context, cmd: &str) -> Result<(i32, String, String)>;
    fn confirm_action(&mut self, message: &str) -> Result<bool>;
    fn is_canceled(&self) -> bool;
    fn run_hook(&mut self, hook_name: &str, args: Vec<String>) -> Result<()>;
    fn select_item(&mut self, items: Vec<String>) -> Result<Option<String>>;
    fn open_editor(&mut self, content: &str, extension: &str) -> Result<String>;
}

impl<T: ShellProxy + ?Sized> ShellExecution for T {
    fn exit_shell(&mut self) {
        ShellProxy::exit_shell(self);
    }

    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()> {
        ShellProxy::dispatch(self, ctx, cmd, argv)
    }

    fn dispatch_core_action(
        &mut self,
        ctx: &Context,
        action: CoreShellAction,
        argv: Vec<String>,
    ) -> Result<()> {
        ShellProxy::dispatch_core_action(self, ctx, action, argv)
    }

    fn request_eval_command(&mut self, command: String) -> Result<()> {
        ShellProxy::request_eval_command(self, command)
    }

    fn capture_command(&mut self, ctx: &Context, cmd: &str) -> Result<(i32, String, String)> {
        ShellProxy::capture_command(self, ctx, cmd)
    }

    fn confirm_action(&mut self, message: &str) -> Result<bool> {
        ShellProxy::confirm_action(self, message)
    }

    fn is_canceled(&self) -> bool {
        ShellProxy::is_canceled(self)
    }

    fn run_hook(&mut self, hook_name: &str, args: Vec<String>) -> Result<()> {
        ShellProxy::run_hook(self, hook_name, args)
    }

    fn select_item(&mut self, items: Vec<String>) -> Result<Option<String>> {
        ShellProxy::select_item(self, items)
    }

    fn open_editor(&mut self, content: &str, extension: &str) -> Result<String> {
        ShellProxy::open_editor(self, content, extension)
    }
}

/// Working-directory, directory-stack, frecency, and directory-alias access.
pub trait ShellNavigation {
    fn save_path_history(&mut self, path: &str);
    fn changepwd(&mut self, path: &str) -> Result<()>;
    fn dir_stack(&self) -> Vec<String>;
    fn dir_stack_set(&mut self, stack: Vec<String>);
    fn get_current_dir(&self) -> Result<PathBuf>;
    fn add_dir_alias(&mut self, name: String, path: String) -> bool;
    fn remove_dir_alias(&mut self, name: &str) -> bool;
    fn list_dir_aliases(&self) -> Vec<(String, String)>;
    fn get_dir_alias(&self, name: &str) -> Option<String>;
}

impl<T: ShellProxy + ?Sized> ShellNavigation for T {
    fn save_path_history(&mut self, path: &str) {
        ShellProxy::save_path_history(self, path);
    }

    fn changepwd(&mut self, path: &str) -> Result<()> {
        ShellProxy::changepwd(self, path)
    }

    fn dir_stack(&self) -> Vec<String> {
        ShellProxy::dir_stack(self)
    }

    fn dir_stack_set(&mut self, stack: Vec<String>) {
        ShellProxy::dir_stack_set(self, stack);
    }

    fn get_current_dir(&self) -> Result<PathBuf> {
        ShellProxy::get_current_dir(self)
    }

    fn add_dir_alias(&mut self, name: String, path: String) -> bool {
        ShellProxy::add_dir_alias(self, name, path)
    }

    fn remove_dir_alias(&mut self, name: &str) -> bool {
        ShellProxy::remove_dir_alias(self, name)
    }

    fn list_dir_aliases(&self) -> Vec<(String, String)> {
        ShellProxy::list_dir_aliases(self)
    }

    fn get_dir_alias(&self, name: &str) -> Option<String> {
        ShellProxy::get_dir_alias(self, name)
    }
}

/// Variables, exported environment, aliases, abbreviations, and policy state.
pub trait ShellEnvironment {
    fn insert_path(&mut self, index: usize, path: &str);
    fn get_var(&mut self, key: &str) -> Option<String>;
    fn set_var(&mut self, key: String, value: String);
    fn set_env_var(&mut self, key: String, value: String);
    fn is_direnv_allowed(&self, path: &Path) -> bool;
    fn unset_env_var(&mut self, key: &str);
    fn get_alias(&mut self, name: &str) -> Option<String>;
    fn set_alias(&mut self, name: String, command: String);
    fn list_aliases(&mut self) -> HashMap<String, String>;
    fn add_abbr(&mut self, name: String, expansion: String);
    fn remove_abbr(&mut self, name: &str) -> bool;
    fn list_abbrs(&self) -> Vec<(String, String)>;
    fn get_abbr(&self, name: &str) -> Option<String>;
    fn list_execute_allowlist(&mut self) -> Vec<String>;
    fn list_exported_vars(&self) -> Vec<(String, String)>;
    fn export_var(&mut self, key: &str) -> bool;
    fn set_and_export_var(&mut self, key: String, value: String);
    fn get_lisp_var(&self, key: &str) -> Option<String>;
    fn safety_level(&mut self) -> SafetyLevel;
}

impl<T: ShellProxy + ?Sized> ShellEnvironment for T {
    fn insert_path(&mut self, index: usize, path: &str) {
        ShellProxy::insert_path(self, index, path);
    }

    fn get_var(&mut self, key: &str) -> Option<String> {
        ShellProxy::get_var(self, key)
    }

    fn set_var(&mut self, key: String, value: String) {
        ShellProxy::set_var(self, key, value);
    }

    fn set_env_var(&mut self, key: String, value: String) {
        ShellProxy::set_env_var(self, key, value);
    }

    fn is_direnv_allowed(&self, path: &Path) -> bool {
        ShellProxy::is_direnv_allowed(self, path)
    }

    fn unset_env_var(&mut self, key: &str) {
        ShellProxy::unset_env_var(self, key);
    }

    fn get_alias(&mut self, name: &str) -> Option<String> {
        ShellProxy::get_alias(self, name)
    }

    fn set_alias(&mut self, name: String, command: String) {
        ShellProxy::set_alias(self, name, command);
    }

    fn list_aliases(&mut self) -> HashMap<String, String> {
        ShellProxy::list_aliases(self)
    }

    fn add_abbr(&mut self, name: String, expansion: String) {
        ShellProxy::add_abbr(self, name, expansion);
    }

    fn remove_abbr(&mut self, name: &str) -> bool {
        ShellProxy::remove_abbr(self, name)
    }

    fn list_abbrs(&self) -> Vec<(String, String)> {
        ShellProxy::list_abbrs(self)
    }

    fn get_abbr(&self, name: &str) -> Option<String> {
        ShellProxy::get_abbr(self, name)
    }

    fn list_execute_allowlist(&mut self) -> Vec<String> {
        ShellProxy::list_execute_allowlist(self)
    }

    fn list_exported_vars(&self) -> Vec<(String, String)> {
        ShellProxy::list_exported_vars(self)
    }

    fn export_var(&mut self, key: &str) -> bool {
        ShellProxy::export_var(self, key)
    }

    fn set_and_export_var(&mut self, key: String, value: String) {
        ShellProxy::set_and_export_var(self, key, value);
    }

    fn get_lisp_var(&self, key: &str) -> Option<String> {
        ShellProxy::get_lisp_var(self, key)
    }

    fn safety_level(&mut self) -> SafetyLevel {
        ShellProxy::safety_level(self)
    }
}

/// Output history, command blocks, snippets, bookmarks, and session history.
pub trait ShellSessionData {
    fn save_output_history(&mut self, entry: OutputEntry);
    fn get_full_output_history(&self) -> Vec<OutputEntry>;
    fn clear_output_history(&mut self) -> usize;
    fn get_command_blocks(&self) -> Vec<CommandBlock>;
    fn clear_command_blocks(&mut self) -> usize;
    fn add_snippet(&mut self, name: String, command: String, description: Option<String>) -> bool;
    fn remove_snippet(&mut self, name: &str) -> bool;
    fn list_snippets(&self) -> Vec<Snippet>;
    fn get_snippet(&self, name: &str) -> Option<Snippet>;
    fn update_snippet(&mut self, name: &str, command: &str, description: Option<&str>) -> bool;
    fn record_snippet_use(&mut self, name: &str);
    fn add_bookmark(&mut self, name: String, command: String) -> bool;
    fn remove_bookmark(&mut self, name: &str) -> bool;
    fn list_bookmarks(&self) -> Vec<(String, String, i64)>;
    fn get_bookmark(&self, name: &str) -> Option<(String, i64)>;
    fn record_bookmark_use(&mut self, name: &str);
    fn get_last_command(&self) -> Option<String>;
    fn command_history_len(&self) -> Option<usize>;
}

impl<T: ShellProxy + ?Sized> ShellSessionData for T {
    fn save_output_history(&mut self, entry: OutputEntry) {
        ShellProxy::save_output_history(self, entry);
    }

    fn get_full_output_history(&self) -> Vec<OutputEntry> {
        ShellProxy::get_full_output_history(self)
    }

    fn clear_output_history(&mut self) -> usize {
        ShellProxy::clear_output_history(self)
    }

    fn get_command_blocks(&self) -> Vec<CommandBlock> {
        ShellProxy::get_command_blocks(self)
    }

    fn clear_command_blocks(&mut self) -> usize {
        ShellProxy::clear_command_blocks(self)
    }

    fn add_snippet(&mut self, name: String, command: String, description: Option<String>) -> bool {
        ShellProxy::add_snippet(self, name, command, description)
    }

    fn remove_snippet(&mut self, name: &str) -> bool {
        ShellProxy::remove_snippet(self, name)
    }

    fn list_snippets(&self) -> Vec<Snippet> {
        ShellProxy::list_snippets(self)
    }

    fn get_snippet(&self, name: &str) -> Option<Snippet> {
        ShellProxy::get_snippet(self, name)
    }

    fn update_snippet(&mut self, name: &str, command: &str, description: Option<&str>) -> bool {
        ShellProxy::update_snippet(self, name, command, description)
    }

    fn record_snippet_use(&mut self, name: &str) {
        ShellProxy::record_snippet_use(self, name);
    }

    fn add_bookmark(&mut self, name: String, command: String) -> bool {
        ShellProxy::add_bookmark(self, name, command)
    }

    fn remove_bookmark(&mut self, name: &str) -> bool {
        ShellProxy::remove_bookmark(self, name)
    }

    fn list_bookmarks(&self) -> Vec<(String, String, i64)> {
        ShellProxy::list_bookmarks(self)
    }

    fn get_bookmark(&self, name: &str) -> Option<(String, i64)> {
        ShellProxy::get_bookmark(self, name)
    }

    fn record_bookmark_use(&mut self, name: &str) {
        ShellProxy::record_bookmark_use(self, name);
    }

    fn get_last_command(&self) -> Option<String> {
        ShellProxy::get_last_command(self)
    }

    fn command_history_len(&self) -> Option<usize> {
        ShellProxy::command_history_len(self)
    }
}

/// Runtime status and diagnostic probes consumed by status and doctor builtins.
pub trait ShellDiagnostics {
    fn get_github_status(&self) -> (usize, usize, usize);
    fn get_git_branch(&self) -> Option<String>;
    fn get_job_count(&self) -> usize;
    fn executable_cache_len(&self) -> Option<usize>;
    fn completion_diagnostics(&self) -> Vec<String>;
    fn latency_probe_lines(&self, iterations: usize) -> Vec<String>;
}

impl<T: ShellProxy + ?Sized> ShellDiagnostics for T {
    fn get_github_status(&self) -> (usize, usize, usize) {
        ShellProxy::get_github_status(self)
    }

    fn get_git_branch(&self) -> Option<String> {
        ShellProxy::get_git_branch(self)
    }

    fn get_job_count(&self) -> usize {
        ShellProxy::get_job_count(self)
    }

    fn executable_cache_len(&self) -> Option<usize> {
        ShellProxy::executable_cache_len(self)
    }

    fn completion_diagnostics(&self) -> Vec<String> {
        ShellProxy::completion_diagnostics(self)
    }

    fn latency_probe_lines(&self, iterations: usize) -> Vec<String> {
        ShellProxy::latency_probe_lines(self, iterations)
    }
}

/// AI and MCP integration points that may perform asynchronous work.
pub trait ShellAiIntegration {
    fn list_mcp_servers(&mut self) -> Vec<McpServerConfig>;
    fn generate_command_completion_async<'a>(
        &'a mut self,
        command_name: &'a str,
        help_text: &'a str,
    ) -> ProxyFuture<'a, String>;
    fn ask_ai_async<'a>(&'a mut self, messages: Vec<serde_json::Value>) -> ProxyFuture<'a, String>;
}

impl<T: ShellProxy + ?Sized> ShellAiIntegration for T {
    fn list_mcp_servers(&mut self) -> Vec<McpServerConfig> {
        ShellProxy::list_mcp_servers(self)
    }

    fn generate_command_completion_async<'a>(
        &'a mut self,
        command_name: &'a str,
        help_text: &'a str,
    ) -> ProxyFuture<'a, String> {
        ShellProxy::generate_command_completion_async(self, command_name, help_text)
    }

    fn ask_ai_async<'a>(&'a mut self, messages: Vec<serde_json::Value>) -> ProxyFuture<'a, String> {
        ShellProxy::ask_ai_async(self, messages)
    }
}

/// A prompt whose response must parse as a single JSON object, unlike
/// [`ShellAiIntegration::ask_ai_async`]'s prose.
///
/// A new independent trait rather than a `ShellAiIntegration`/`ShellProxy`
/// method: `ShellProxy::ask_ai_async` is used by three prose callers
/// (`blocks::ai`, `blocks::export`) that must keep getting `AI_MESSAGE_LANG`
/// applied to their system prompt, so the JSON-mode request needs its own
/// path rather than a parameter that would force a choice on every caller.
/// `output-gen`'s AI-generated output-schema is the one caller: its system
/// prompt asks for a JSON object whose `type`/`parse`/`separator` fields are
/// enum values, and `AI_MESSAGE_LANG` translating those broke the parse
/// (`docs/ai/skills/doge-shell-repo/references/ai/env-vars.md` §6: "JSON を
/// 返させるリクエストに `apply_language` を付けない").
pub trait AiJsonRequest {
    fn ask_ai_json_async<'a>(
        &'a mut self,
        messages: Vec<serde_json::Value>,
    ) -> ProxyFuture<'a, String>;
}

/// Job-control wait operations owned by the shell core.
///
/// `wait PID` reports the waited child's status itself — not just success
/// or failure — so it cannot travel through [`crate::CoreShellAction`]
/// (whose dispatch shape is `Result<()>`). Like `AgentCommandPolicy`, this
/// is a supertrait bound on [`crate::ShellProxy`], not a new facade
/// method: no entry is added to the frozen compatibility surface.
pub trait JobControlCapability {
    fn wait_for_jobs(&mut self, ctx: &Context, argv: Vec<String>) -> Result<i32>;
}

/// POSIX `set -o` / `set +o` option state owned by the shell core.
///
/// A supertrait bound on [`crate::ShellProxy`], not a new facade method:
/// no entry is added to the frozen compatibility surface. Builtins reach
/// option state only through this capability (`set.rs`), and both the real
/// `Shell` and `TestShellProxy` implement it explicitly.
pub trait ShellOptionCapability {
    fn shell_option_enabled(&self, option: ShellOption) -> bool;

    fn set_shell_option(&mut self, option: ShellOption, enabled: bool);
}

/// Single-line `read NAME` owned by the shell core.
///
/// `read` reports its command status itself (success vs. EOF vs. usage
/// error) — not just success or failure — so it cannot travel through
/// [`crate::CoreShellAction`] (whose dispatch shape is `Result<()>`).
/// Like `JobControlCapability`, this is a supertrait bound on
/// [`crate::ShellProxy`], not a new facade method: no entry is added to
/// the frozen compatibility surface.
pub trait ReadCapability {
    fn read_shell_line(
        &mut self,
        ctx: &Context,
        argv: Vec<String>,
    ) -> anyhow::Result<dsh_types::ExitStatus>;
}

/// Child-visible environment owned by the shell core.
///
/// Returns the materialized child environment (`variables` filtered by
/// `exported_vars`): what a `bash` spawned for `include` must inherit via
/// `env_clear` + `envs`, never the process-global `std::env`.
///
/// Like `JobControlCapability`, this is a supertrait bound on
/// [`crate::ShellProxy`], not a new facade method: no entry is added to
/// the frozen compatibility surface. Both the real `Shell` and
/// `TestShellProxy` implement it explicitly.
pub trait ProcessEnvironmentCapability {
    fn child_process_environment(&self) -> HashMap<String, String>;
    /// Logical command search paths (`Environment.variable_state.paths`).
    ///
    /// The only authority for runtime executable lookup (task providers,
    /// command-not-found suggestions). Never falls back to `std::env::PATH`,
    /// so a test can pin PATH A vs PATH B without touching process-global
    /// state.
    fn command_search_paths(&self) -> Vec<PathBuf>;
    /// Immutable runtime snapshot for one child spawn: logical search
    /// paths, exported child env, snapshot cwd — never process-global
    /// `std::env`. An independent capability method, not a new `ShellProxy`
    /// facade method.
    fn command_runtime_snapshot(
        &self,
    ) -> Result<dsh_types::process_runtime::CommandRuntimeSnapshot>;
}

/// What the shell's safety policy says about a command the agent wants to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommandVerdict {
    /// Run it without asking.
    Allowed,
    /// Ask the user first; the string explains why.
    Confirm(String),
    /// Refuse; the string explains why.
    Denied(String),
}

/// What the user chose when asked to approve a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Allow,
    /// Allow, and stop asking about this command for the rest of the session.
    AllowAlways,
    Deny,
}

/// The shell's command policy, as the chat agent's `execute` tool needs it.
///
/// Deliberately outside [`ShellProxy`]: that facade is frozen as a
/// compatibility layer, and this is a new dependency rather than an old one.
/// The agent needs three things the facade cannot express - a verdict on a full
/// command line including its pipeline, a three-way answer from the user
/// (`ShellProxy::confirm_action` only returns a bool, so "always" was
/// unreachable), and a permission set that is the agent's own rather than the
/// user's.
pub trait AgentCommandPolicy {
    /// Optional durable task context. Ordinary `!` hosts remain unchanged.
    fn agent_runtime(&self) -> Option<Arc<parking_lot::Mutex<crate::agent::AgentRuntime>>> {
        None
    }
    fn evaluate_agent_file(&mut self, _path: &Path, _write: bool) -> AgentCommandVerdict {
        AgentCommandVerdict::Confirm("file access requires approval".into())
    }
    /// Judge a whole command line - pipelines included - against the shell's
    /// safety guard at the current safety level.
    fn evaluate_agent_command(&mut self, command: &str) -> AgentCommandVerdict;

    /// Ask the user, offering "always".
    ///
    /// Without it a twenty-step agent run is twenty prompts, which is how a
    /// safety gate turns into a key people hold down.
    fn request_agent_approval(&mut self, message: &str) -> Result<ApprovalDecision>;

    /// Remember `command` as approved for the rest of this session.
    ///
    /// Stored and matched as the exact line the user saw, never as a prefix:
    /// approving `rm -rf target` must not also approve
    /// `rm -rf target ~/documents`.
    fn remember_agent_approval(&mut self, command: &str);

    /// Command lines approved with "always" this session, matched exactly.
    fn agent_session_approvals(&mut self) -> Vec<String>;

    /// Commands the agent may run without asking, matched by token prefix.
    ///
    /// Configured entries only - `(chat-execute-add ...)`, the JSON config and
    /// the environment variable - because those are written by a person who
    /// meant the prefix.
    fn agent_allowlist(&mut self) -> Vec<String>;

    /// Judge a non-command tool call - an MCP tool - against the same safety
    /// level and allowlist that `evaluate_agent_command` uses.
    ///
    /// Without it the `!` runtime asked about every MCP call unconditionally
    /// while the shell-side loop applied the policy, so the same tool behaved
    /// differently depending on which entry point reached it.
    fn evaluate_agent_tool(&mut self, name: &str, arguments: &str) -> AgentCommandVerdict;

    /// The allowlist entry that an "always" answer about this tool call should
    /// be remembered as. The host owns the spelling, because it is the host
    /// that matches against it.
    fn agent_tool_approval_entry(&mut self, name: &str, arguments: &str) -> String;

    /// The shell's MCP manager - the same one `mcp status` and `mcp connect`
    /// act on.
    ///
    /// The `!` runtime used to build a second manager of its own from the
    /// server configs and cache it for five minutes, so `mcp disconnect` had no
    /// effect on chat and `mcp status` described a different set of
    /// connections from the one the agent was actually using.
    fn agent_mcp_manager(&mut self) -> Arc<RwLock<McpManager>>;
}

/// Persistence is owned by the shell, not by the chat loop or provider.
pub struct AgentTaskSave {
    pub sequence: u64,
    pub task: dsh_types::agent::AgentTask,
}

pub trait AgentTaskStore: Send + Sync {
    fn save(
        &self,
        task: &dsh_types::agent::AgentTask,
        event: Option<(&str, &serde_json::Value)>,
    ) -> Result<AgentTaskSave>;
    /// Explicit user-driven resume is the only operation allowed to clear a
    /// persisted cancellation.
    fn resume(
        &self,
        task: &dsh_types::agent::AgentTask,
        event: Option<(&str, &serde_json::Value)>,
    ) -> Result<AgentTaskSave>;
    fn load(&self, id: &str) -> Result<dsh_types::agent::AgentTask>;
    fn list(&self) -> Result<Vec<dsh_types::agent::AgentTask>>;
    fn events(&self, id: &str) -> Result<Vec<dsh_types::agent::TaskEvent>>;
    fn delete(&self, id: &str) -> Result<()>;
    fn save_artifact(&self, id: &str, name: &str, content: &serde_json::Value) -> Result<()>;
    fn load_artifact(&self, id: &str, name: &str) -> Result<serde_json::Value>;
}

/// Cron's persistence, owned by the shell.
///
/// Split from [`AgentTaskStore`] for the same reason that exists at all:
/// `rusqlite` is a `dsh` dependency, and a builtin is not where durable state
/// belongs. The two stores stay separate rather than sharing one database -
/// the agent store serialises *one* task at a time behind a file lock, while
/// cron needs row-level claims across several processes at once, and those
/// want opposite journal modes.
///
/// Every method takes `now` instead of reading the clock, so a test can drive
/// a schedule years forward without sleeping and without a global time hook.
/// Timestamps are Unix seconds UTC throughout; local time is resolved once,
/// when a schedule is turned into a `next_run_at`, and never stored.
pub trait CronStore: Send + Sync {
    /// Registers a job. Fails on a duplicate name unless `force`, so a typo at
    /// the prompt cannot silently replace a working job.
    fn create(
        &self,
        spec: &CronJobSpec,
        env: &HashMap<String, String>,
        now: i64,
        force: bool,
    ) -> Result<i64>;
    /// Registers or replaces by name, without complaining about a duplicate.
    ///
    /// This is what `config.lisp` uses: it is evaluated on every startup, so
    /// the `create` rule would turn the second launch into an error - and a
    /// `config.lisp` error aborts the rest of the file.
    fn upsert(&self, spec: &CronJobSpec, env: &HashMap<String, String>, now: i64) -> Result<i64>;
    /// Changes only the fields the patch names. Returns the job's name.
    fn patch(&self, selector: &str, patch: &CronJobPatch, now: i64) -> Result<String>;
    fn delete(&self, selector: &str) -> Result<String>;
    fn get(&self, selector: &str) -> Result<CronJobView>;
    fn list(&self) -> Result<Vec<CronJobView>>;
    fn set_paused(&self, selector: &str, paused: bool, now: i64) -> Result<String>;
    /// Pauses or resumes every job. Resuming re-bases `next_run_at` rather than
    /// letting the slots that elapsed while paused all fire at once.
    fn set_all_paused(&self, paused: bool, now: i64) -> Result<usize>;
    /// Makes a job due immediately. Returns its name.
    fn trigger(&self, selector: &str, now: i64) -> Result<String>;
    /// Atomically takes ownership of up to `limit` due jobs.
    ///
    /// The only thing standing between two sessions, an external tick and a
    /// double execution. Advances `next_run_at` in the same transaction, so a
    /// job that is skipped still moves forward instead of spinning.
    fn claim_due(
        &self,
        now: i64,
        owner: &str,
        limit: usize,
        trigger: RunTrigger,
    ) -> Result<Vec<ClaimedRun>>;
    /// Claims one named job immediately, ignoring `next_run_at` (but not an
    /// existing claim): what `cron run --now` and `cron run-job`'s manual
    /// path use to force a specific job rather than whatever the wall clock
    /// says is due.
    fn claim_one(
        &self,
        selector: &str,
        now: i64,
        owner: &str,
        trigger: RunTrigger,
    ) -> Result<ClaimedRun>;
    /// Moves a claimed run to `running` and hands back what to execute.
    fn start(&self, run_id: &str, now: i64) -> Result<ClaimedRun>;
    /// Records which agent task a run started, before it can possibly finish.
    ///
    /// Legacy: only rows written by older agent-job versions carry this.
    /// `complete` is the only other writer of `runs.agent_task_id`, and a run
    /// killed after its lease expired never reaches it: `reap_expired_leases`
    /// closes the row out as `failed`/`timeout` without knowing what task it
    /// had started.
    fn attach_agent_task(&self, run_id: &str, task_id: &str) -> Result<()>;
    /// Records the outcome, releases the claim and opens or closes incidents.
    fn complete(&self, run_id: &str, outcome: &RunOutcome, now: i64) -> Result<()>;
    fn runs(&self, query: &RunQuery) -> Result<Vec<CronRun>>;
    /// One run's full recorded `stdout`/`stderr`, read on demand - see
    /// [`RunOutput`]'s own doc comment for why this is not part of `runs`.
    fn run_output(&self, selector: &RunSelector) -> Result<RunOutput>;
    fn incidents(&self, open_only: bool, limit: usize) -> Result<Vec<CronIncident>>;
    fn open_incident(
        &self,
        job_id: Option<i64>,
        kind: IncidentKind,
        detail: &str,
        agent_task_id: Option<&str>,
        now: i64,
    ) -> Result<i64>;
    /// Acknowledges an incident and unblocks its job when no other blocking
    /// incident remains open.
    fn ack_incident(&self, id: i64, now: i64) -> Result<CronIncident>;
    fn notepad(&self, selector: &str) -> Result<String>;
    fn set_notepad(&self, selector: &str, body: &str) -> Result<()>;
    /// Releases claims whose lease ran out. Cheap enough to call every scan.
    fn reap_expired_leases(&self, now: i64) -> Result<usize>;
    fn health(&self, now: i64) -> Result<CronHealth>;
    /// Tokens this job has spent since `since`, for the rolling daily ceiling.
    fn tokens_used_since(&self, job_id: i64, since: i64) -> Result<u64>;
    /// When the next job comes due, so an idle runner can sleep until then.
    fn next_due_at(&self) -> Result<Option<i64>>;
}

/// One `cron_manage` chat-tool call, dispatched to `cron`'s own argv parser
/// and store.
///
/// Split from [`CronStore`] rather than folded into it: `CronToolRequest`'s
/// fields are unparsed strings (a chat tool's JSON, not a typed spec), and
/// turning them into a [`CronJobSpec`]/[`CronJobPatch`] means calling
/// `dsh/src/cron/cli/parse.rs`'s `parse_add`/`parse_edit` - which live in
/// `dsh`, need `rusqlite` to look up a job's existing agent grant before an
/// edit, and so cannot be reached from `dsh-builtin` any more directly than
/// [`CronStore`]'s own implementation can.
pub trait CronToolHost {
    fn cron_tool_call(&mut self, request: &CronToolRequest) -> Result<serde_json::Value>;
}

/// Everything a chat tool needs from its host.
///
/// A bundle rather than a new [`ShellProxy`] method, so the frozen facade stays
/// the size it is. Trait upcasting lets a `&mut dyn ChatToolHost` be passed
/// wherever a `&mut dyn ShellProxy` is expected.
pub trait ChatToolHost: ShellProxy + AgentCommandPolicy + CronToolHost {}

impl<T: ShellProxy + AgentCommandPolicy + CronToolHost + ?Sized> ChatToolHost for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;

    fn change_directory(proxy: &mut impl ShellNavigation, path: &str) -> Result<()> {
        proxy.changepwd(path)
    }

    struct NavigationFake {
        changed_to: Option<String>,
    }

    impl ShellNavigation for NavigationFake {
        fn save_path_history(&mut self, _path: &str) {}
        fn changepwd(&mut self, path: &str) -> Result<()> {
            self.changed_to = Some(path.to_string());
            Ok(())
        }
        fn dir_stack(&self) -> Vec<String> {
            Vec::new()
        }
        fn dir_stack_set(&mut self, _stack: Vec<String>) {}
        fn get_current_dir(&self) -> Result<PathBuf> {
            Ok(PathBuf::from("/fake"))
        }
        fn add_dir_alias(&mut self, _name: String, _path: String) -> bool {
            false
        }
        fn remove_dir_alias(&mut self, _name: &str) -> bool {
            false
        }
        fn list_dir_aliases(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn get_dir_alias(&self, _name: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn legacy_shell_proxy_receives_capability_adapters() {
        let mut proxy = TestShellProxy {
            current_dir: PathBuf::from("/work"),
            allow_changepwd: true,
            ..TestShellProxy::default()
        };

        change_directory(&mut proxy, "/next").unwrap();
        assert_eq!(proxy.changed_to.as_deref(), Some("/next"));
    }

    #[test]
    fn helper_can_use_capability_only_fake() {
        let mut fake = NavigationFake { changed_to: None };

        change_directory(&mut fake, "/next").unwrap();
        assert_eq!(fake.changed_to.as_deref(), Some("/next"));
    }

    #[test]
    fn execution_capability_keeps_fail_closed_defaults() {
        let mut proxy = TestShellProxy {
            current_dir: PathBuf::from("/work"),
            ..TestShellProxy::default()
        };

        assert!(!ShellExecution::confirm_action(&mut proxy, "dangerous").unwrap());
    }
}
