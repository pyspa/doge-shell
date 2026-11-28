//! Cached filesystem and PATH candidates for the legacy Tab fallback.

use super::{
    Candidate, CompletionFrameworkKind, CompletionSelection, Repl, select_completion_items,
};
use crate::completion::cache::CompletionCache;
use crate::completion::commands::{deduplicate_candidates, get_commands};
use crate::completion::fuzzy::fuzzy_match_score;
use crate::completion::session_commands::session_command_names;
use skim::SkimItem;
use std::collections::BTreeSet;
use std::fs::read_dir;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tracing::debug;

const LEGACY_CACHE_TTL_MS: u64 = 3000;
static LEGACY_COMPLETION_CACHE: LazyLock<CompletionCache<Candidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(LEGACY_CACHE_TTL_MS)));

pub(super) async fn completion_from_current_with_prompt(
    repl: &Repl<'_>,
    snapshot: &LegacyCompletionSnapshot,
    query: Option<&str>,
    raw_token: Option<&str>,
    prompt_text: &str,
    input_text: &str,
    framework: CompletionFrameworkKind,
) -> CompletionSelection {
    let raw_query = query.unwrap_or_default();
    let query = crate::completion::shell_path::decode_literal_word(raw_query);
    let Some(mut data) =
        cached_current_context_candidates(snapshot, raw_query, &LEGACY_COMPLETION_CACHE).await
    else {
        return CompletionSelection::None;
    };
    if snapshot.command && !query.contains('/') && query != "~" {
        let mut names = session_command_names(
            repl.shell,
            raw_token
                .is_none_or(|raw| crate::completion::shell_path::decode_literal_word(raw) == raw),
            |name| fuzzy_match_score(name, &query).is_some(),
        );
        names.append(&mut data.items);
        data.items = deduplicate_candidates(names);
        if data
            .items
            .iter()
            .any(|item| item.output().starts_with(&query))
        {
            data.items.retain(|item| item.output().starts_with(&query));
        }
    }
    select_completion_items(
        data.items,
        data.selection_query.as_deref(),
        prompt_text,
        input_text,
        framework,
    )
}

/// The cache identity and collection inputs come from the same logical state.
/// This static cache also serves distinct Environment instances, so PATH's
/// actual values are included alongside its per-Environment generation.
pub(super) struct LegacyCompletionSnapshot {
    runtime: dsh_types::process_runtime::CommandRuntimeSnapshot,
    home: Option<String>,
    pub(super) path_generation: u64,
    command: bool,
    pub(super) system: Option<(
        crate::completion::generator::SystemCommandCacheTicket,
        Arc<crate::completion::dynamic::CompletionRuntime>,
    )>,
}

impl LegacyCompletionSnapshot {
    pub(super) fn capture(repl: &Repl<'_>, cwd: &Path, command: bool) -> Self {
        let environment = repl.shell.environment.read();
        let mut snapshot =
            Self::from_environment(&environment, cwd.to_path_buf()).for_command(command);
        if command && environment.invocation.interactive {
            let filesystem = crate::completion::generators::filesystem::FileSystemContext::new(
                cwd,
                snapshot.home.clone(),
            );
            let ticket = crate::completion::generators::system::activate_system_command_cache_at(
                &environment.variable_state.paths,
                &filesystem,
            );
            snapshot.system = Some((ticket, repl.completion_ui.integrated_completion.runtime()));
        }
        snapshot
    }

    pub(super) fn from_environment(
        environment: &crate::environment::Environment,
        cwd: PathBuf,
    ) -> Self {
        Self {
            runtime: environment.command_runtime_snapshot(cwd),
            home: environment.lookup_variable("HOME"),
            path_generation: environment.completion_state.path_generation,
            command: false,
            system: None,
        }
    }

    pub(super) fn for_command(mut self, command: bool) -> Self {
        self.command = command;
        self
    }

    pub(super) fn cache_scope(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut scope = std::collections::hash_map::DefaultHasher::new();
        self.runtime.current_dir().hash(&mut scope);
        self.home.hash(&mut scope);
        self.command.hash(&mut scope);
        self.runtime.command_search_paths().hash(&mut scope);
        self.path_generation.hash(&mut scope);
        if self.system.is_some() {
            crate::completion::generators::system::system_command_cache_revision().hash(&mut scope);
        }
        scope.finish()
    }

    fn expand_home(&self, raw_query: &str) -> String {
        let query = crate::completion::shell_path::decode_literal_word(raw_query);
        if (raw_query == "~" || raw_query.starts_with("~/"))
            && let Some(home) = &self.home
        {
            format!("{home}{}", &query[1..])
        } else {
            // Logical unset is final: do not rediscover process-global HOME.
            query
        }
    }
}

pub(super) async fn cached_current_context_candidates(
    snapshot: &LegacyCompletionSnapshot,
    raw_query: &str,
    cache: &CompletionCache<Candidate>,
) -> Option<CurrentCompletionData> {
    let scope = snapshot.cache_scope();
    // A longer query can cross into an explicit path or match a different
    // subset of fuzzy candidates, so cache only the exact source spelling.
    if let Some(hit) = cache.lookup_scoped_exact(scope, raw_query) {
        return Some(CurrentCompletionData {
            items: hit.candidates,
            selection_query: Some(snapshot.expand_home(raw_query)),
        });
    }
    let data = collect_current_context_candidates(snapshot, raw_query).await?;
    if !data.items.is_empty() {
        cache.set_scoped(scope, raw_query.to_string(), data.items.clone());
    }
    Some(data)
}

#[derive(Debug, Clone)]
pub(super) struct CurrentCompletionData {
    pub(super) items: Vec<Candidate>,
    selection_query: Option<String>,
}

pub(super) async fn collect_current_context_candidates(
    snapshot: &LegacyCompletionSnapshot,
    raw_query: &str,
) -> Option<CurrentCompletionData> {
    let current_dir = snapshot.runtime.current_dir();
    let query = crate::completion::shell_path::decode_literal_word(raw_query);
    let expanded = snapshot.expand_home(raw_query);
    let expanded_path = Path::new(&expanded);
    let lookup_path = current_dir.join(expanded_path);

    // Determine the path to search and query substring
    let (search_path, path_query, only_path) = if !expanded.is_empty() && lookup_path.is_dir() {
        (expanded_path.to_path_buf(), String::new(), true)
    } else if let Some(parent) = expanded_path.parent() {
        let parent_buf = parent.to_path_buf();
        let has_parent = !parent_buf.as_os_str().is_empty();
        if let Some(file_name) = expanded_path.file_name()
            && let Some(file_name_str) = file_name.to_str()
        {
            (parent_buf, file_name_str.to_string(), has_parent)
        } else {
            (expanded_path.to_path_buf(), String::new(), has_parent)
        }
    } else {
        (PathBuf::new(), query.clone(), false)
    };

    // Canonicalize the path for consistent resolution
    let lookup_dir = current_dir.join(&search_path);
    let canonical_path = lookup_dir.canonicalize().unwrap_or(lookup_dir);
    let canonical_str = canonical_path.display().to_string();
    let search_path_str = search_path.to_str()?.to_string();

    // Bare command names resolve through the session/PATH. Directories also
    // work through implicit cd. Files need an explicit path here.
    let command_files = snapshot.command.then_some(query.contains('/'));
    let mut items = get_file_completions_with_filter(
        &canonical_str,
        &search_path_str,
        (!path_query.is_empty()).then_some(path_query.as_str()),
        command_files,
    )
    .await;

    if snapshot.command && !only_path {
        let query_str = query;

        let mut command_items = if let Some((ticket, runtime)) = &snapshot.system {
            crate::completion::generators::system::SystemCommandGenerator::from_activation(
                ticket.clone(),
            )
            .generate_in_background(&query_str, runtime.clone(), usize::MAX)
            .into_iter()
            .map(|candidate| Candidate::Item(candidate.text, "(command)".into()))
            .collect()
        } else {
            // Anchor relative/empty PATH entries before crossing the worker boundary.
            let paths: Vec<String> = snapshot
                .runtime
                .command_search_paths()
                .iter()
                .map(|path| current_dir.join(path).to_string_lossy().into_owned())
                .collect();
            tokio::task::spawn_blocking(move || get_commands(&paths, &query_str))
                .await
                .unwrap_or_default()
        };

        items.append(&mut command_items);
        items = deduplicate_candidates(items);
    }

    Some(CurrentCompletionData {
        items,
        selection_query: Some(path_query),
    })
}

async fn get_file_completions_with_filter(
    dir: &str,
    prefix: &str,
    filter_prefix: Option<&str>,
    command_files: Option<bool>,
) -> Vec<Candidate> {
    let dir_owned = dir.to_string();
    let prefix_owned = prefix.to_string();
    let filter_prefix_owned = filter_prefix.map(|s| s.to_string());

    tokio::task::spawn_blocking(move || {
        collect_legacy_files(
            &dir_owned,
            &prefix_owned,
            filter_prefix_owned.as_deref(),
            command_files,
        )
    })
    .await
    .unwrap_or_default()
}

#[cfg(test)]
pub(super) fn get_file_completions_with_filter_sync(
    dir: &str,
    prefix: &str,
    filter_prefix: Option<&str>,
) -> Vec<Candidate> {
    collect_legacy_files(dir, prefix, filter_prefix, None)
}

fn collect_legacy_files(
    dir: &str,
    prefix: &str,
    filter_prefix: Option<&str>,
    command_files: Option<bool>,
) -> Vec<Candidate> {
    debug!(
        "get_file_completions_with_filter: dir={}, prefix={}, filter_prefix={:?}",
        dir, prefix, filter_prefix
    );
    let mut candidates_set = BTreeSet::new();
    let prefix = if !prefix.is_empty() && !prefix.ends_with('/') {
        format!("{prefix}/")
    } else {
        prefix.to_string()
    };

    debug!("reading directory: {}", dir);
    match read_dir(dir) {
        Ok(entries) => {
            let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
            entries.sort_by_key(|x| x.file_name());

            for entry in entries {
                // Handle potential errors when getting file name
                let file_name_os = entry.file_name();
                let file_name = match file_name_os.to_str() {
                    Some(name) => name,
                    None => {
                        // Skip entries with non-UTF-8 names
                        continue;
                    }
                };

                // Handle potential errors when getting file type
                // Apply prefix filter if provided
                if let Some(filter) = filter_prefix
                    && fuzzy_match_score(file_name, filter).is_none()
                {
                    continue;
                }

                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let is_dir =
                    file_type.is_dir() || (file_type.is_symlink() && entry.path().is_dir());
                if !is_dir
                    && let Some(explicit) = command_files
                    && (!explicit || !crate::environment::is_executable_file(&entry.path()))
                {
                    continue;
                }
                let candidate = if !is_dir {
                    Candidate::Item(format!("{prefix}{file_name}"), "(file)".to_string())
                } else {
                    Candidate::Item(format!("{prefix}{file_name}"), "(directory)".to_string())
                };

                // BTreeSet automatically handles deduplication
                candidates_set.insert(candidate);
            }
        }
        Err(_err) => {}
    }

    // Convert BTreeSet back to Vec to maintain the expected return type
    candidates_set.into_iter().collect()
}
