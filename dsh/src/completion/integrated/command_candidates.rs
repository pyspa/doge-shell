//! Completing the command position and the tokens that behave the same way
//! whatever the command is (`$VAR`, `~user`), plus the one-level unwrap that
//! lets `sudo pacman -R <TAB>` reach pacman's own completion.
use super::*;

impl IntegratedCompletionEngine {
    /// Collect candidates for "special" tokens that complete the same way
    /// regardless of the command: environment/shell variable references
    /// (`$VAR`, `${VAR`) and user-home references (`~user`). Returns `None`
    /// when the current token is not one of these, so normal completion
    /// proceeds unchanged.
    pub(super) fn collect_special_token_candidates(
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
    pub(super) fn variable_candidates(
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

    pub(super) fn collect_command_candidates(
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
    pub(super) fn unwrap_command_with_args_once(
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
}
