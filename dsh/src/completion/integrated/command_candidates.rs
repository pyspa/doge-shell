//! Command candidates, shell state snapshots and wrapper lookup.
use super::*;
use crate::completion::generator::SystemCommandCacheTicket;

impl IntegratedCompletionEngine {
    /// Capture all shell state used by JSON/system completion under one read
    /// lock, so one collection cannot mix variable names from before a mutation
    /// with executable paths from after it.
    pub(super) fn runtime_completion_snapshot(
        &self,
        current_dir: &Path,
    ) -> (Vec<String>, SystemCommandCacheTicket, FileSystemContext) {
        let environment = self.environment.read();
        let names = environment
            .variable_state
            .variables
            .keys()
            .cloned()
            .collect();
        let filesystem = FileSystemContext::new(current_dir, environment.lookup_variable("HOME"));
        let system_command_ticket =
            crate::completion::generators::system::activate_system_command_cache_at(
                &environment.variable_state.paths,
                &filesystem,
            );
        (names, system_command_ticket, filesystem)
    }

    pub(super) fn filesystem_context(&self, current_dir: &Path) -> FileSystemContext {
        FileSystemContext::new(current_dir, self.environment.read().lookup_variable("HOME"))
    }

    pub(super) fn path_refresh_runtime(&self) -> Option<Arc<CompletionRuntime>> {
        self.environment
            .read()
            .invocation
            .interactive
            .then(|| self.runtime.clone())
    }

    pub(super) fn current_cache_scope(&self, current_dir: &Path) -> u64 {
        use std::hash::{Hash, Hasher};
        let environment = self.environment.read();
        let mut scope = std::collections::hash_map::DefaultHasher::new();
        environment
            .completion_state
            .path_generation
            .hash(&mut scope);
        if environment.invocation.interactive {
            crate::completion::generators::system::system_command_cache_revision().hash(&mut scope);
        }
        environment.lookup_variable("HOME").hash(&mut scope);
        aliases::alias_signature(&environment.variable_state.alias).hash(&mut scope);
        if !current_dir.is_absolute() {
            std::env::current_dir().ok().hash(&mut scope);
        }
        current_dir.hash(&mut scope);
        scope.finish()
    }

    /// Build variable-name candidates from the shell environment. Names come
    /// from shell variables only (inherited names are already seeded there),
    /// deduplicated and sorted. `format_value` renders the
    /// final replacement text (e.g. `$NAME` or `${NAME}`).
    pub(super) fn variable_candidates(
        &self,
        prefix: &str,
        format_value: impl Fn(&str) -> String,
    ) -> Vec<EnhancedCandidate> {
        let env = self.environment.read();
        let mut names: Vec<_> = reference::matching_variable_names(&env, prefix).collect();
        names.sort_unstable();
        names
            .into_iter()
            .map(|name| EnhancedCandidate {
                text: format_value(name),
                description: Some("environment variable".to_string()),
                candidate_type: CandidateType::VariableReference,
                priority: 140,
            })
            .collect()
    }

    pub(super) fn collect_command_candidates(
        &self,
        request: &CompletionRequest,
        parsed_command_line: &parser::ParsedCommandLine,
    ) -> CandidateBatch {
        let target = self.dynamic_provider_target(parsed_command_line);
        let parsed_command_line = target.as_ref();
        if parsed_command_line.completion_context == parser::CompletionContext::Command {
            debug!("No completion context found - skipping JSON completion");
            return CandidateBatch::empty();
        }

        self.ensure_command_completion_loaded(&parsed_command_line.command);

        let (environment_names, system_command_ticket, filesystem) =
            self.runtime_completion_snapshot(request.current_dir);
        let db_lock = self.command_completion.lock();

        let completion_generator = CompletionGenerator::with_runtime_environment_ticket(
            &db_lock,
            &environment_names,
            system_command_ticket.clone(),
        )
        .with_filesystem_context(&filesystem)
        .with_background_refresh(self.path_refresh_runtime());

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

                CandidateBatch::inclusive_with_framework(
                    enhanced_candidates,
                    CompletionFrameworkKind::Skim,
                )
            }
            // Add retry logic for lazy loading of inner commands
            Err(crate::completion::generator::GeneratorError::MissingCommand(cmd)) => {
                // Drop lock to load
                drop(db_lock);
                debug!("Generator requested lazy load for command: {}", cmd);

                if let Some(loader) = &self.loader {
                    match loader.load_command_completion(&cmd) {
                        Ok(Some(completion)) => {
                            self.command_completion.lock().add_command(completion);

                            // Retry generation with loaded command
                            let db_lock = self.command_completion.lock();
                            let completion_generator =
                                CompletionGenerator::with_runtime_environment_ticket(
                                    &db_lock,
                                    &environment_names,
                                    system_command_ticket.clone(),
                                )
                                .with_filesystem_context(&filesystem)
                                .with_background_refresh(self.path_refresh_runtime());
                            match completion_generator.generate_candidates(parsed_command_line) {
                                Ok(candidates) => {
                                    let enhanced_candidates = candidates
                                        .into_iter()
                                        .map(|c| self.convert_to_enhanced_candidate(c))
                                        .collect::<Vec<_>>();

                                    return CandidateBatch::inclusive_with_framework(
                                        enhanced_candidates,
                                        CompletionFrameworkKind::Skim,
                                    );
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
                let completion_generator = CompletionGenerator::with_runtime_environment_ticket(
                    &db_lock,
                    &environment_names,
                    system_command_ticket.clone(),
                )
                .with_filesystem_context(&filesystem)
                .with_background_refresh(self.path_refresh_runtime());
                if let Ok(candidates) = completion_generator
                    .generate_fallback_candidates(&parsed_command_line.current_token)
                {
                    let enhanced_candidates = candidates
                        .into_iter()
                        .map(|c| self.convert_to_enhanced_candidate(c))
                        .collect();
                    CandidateBatch::inclusive_with_framework(
                        enhanced_candidates,
                        CompletionFrameworkKind::Skim,
                    )
                } else {
                    CandidateBatch::empty()
                }
            }
            Err(e) => {
                warn!("Failed to generate JSON completion candidates: {}", e);
                CandidateBatch::empty()
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
    pub(super) fn unwrap_command_with_args_once(
        &self,
        parsed: &parser::ParsedCommandLine,
    ) -> Option<parser::ParsedCommandLine> {
        self.ensure_command_completion_loaded(&parsed.command);

        let mut inner = {
            let db_lock = self.command_completion.lock();
            let corrector = ContextCorrector::new(&db_lock);
            let (cmd_index, cmd_name) = corrector.find_command_with_args_arg(parsed)?;
            let follows = if let Some(source) = &parsed.source {
                corrector
                    .argument_raw_index(parsed, cmd_index)
                    .is_some_and(|raw_index| {
                        let offset = source.words.len().saturating_sub(parsed.raw_args.len());
                        source.cursor_word > offset + raw_index
                    })
            } else {
                cursor_follows_wrapped_command(parsed, &cmd_name)
            };
            if !follows {
                return None;
            }
            corrector.reparse_inner_command(parsed, cmd_index, cmd_name)
        };

        // Check that the wrapper was consumed before normalization: an alias
        // can add fixed arguments, so its final argv need not be shorter.
        let consumed_wrapper = match (&inner.source, &parsed.source) {
            (Some(inner), Some(outer)) => inner.input.len() < outer.input.len(),
            _ => inner.raw_args.len() < parsed.raw_args.len(),
        };
        if inner.command.is_empty() || !consumed_wrapper {
            return None;
        }

        self.normalize_parsed_command_line(&mut inner);
        Some(inner)
    }
}
