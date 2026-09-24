//! The ghost-text path: a single best continuation for the line as typed,
//! which is why it only consults sources that are already cheap or cached and
//! never blocks the key handler on a refresh.
use super::*;

impl IntegratedCompletionEngine {
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

        // Variable / user-home references: predict the first matching candidate.
        if let Some(candidates) =
            self.collect_special_token_candidates(&parsed_command_line.current_token)
        {
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
            return Some(full);
        }

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

    pub(super) fn collect_command_candidates_for_ghost(
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
        let (environment_names, system_command_ticket) = self.runtime_completion_snapshot();
        let db_lock = self.command_completion.lock();
        if db_lock.get_command(&parsed_command_line.command).is_none() {
            return Vec::new();
        }

        let completion_generator = CompletionGenerator::with_runtime_environment_ticket(
            &db_lock,
            &environment_names,
            system_command_ticket,
        );
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

    pub(super) fn collect_safe_value_candidates_for_ghost(
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
            if provider == "shell.job" {
                return self.collect_shell_job_candidates(&parsed_command_line.current_token);
            }
            return self.dynamic.collect_declared_dynamic_candidates(
                &provider,
                scope.as_deref(),
                parsed_command_line,
                current_dir,
                CachePolicy::CachedOnly,
            );
        }

        if !is_ghost_safe_argument_type(&arg_type) {
            return Vec::new();
        }

        let (environment_names, system_command_ticket) = self.runtime_completion_snapshot();
        let generator = ArgumentGenerator::with_runtime_environment_ticket(
            &db_lock,
            &environment_names,
            Some(system_command_ticket),
        );
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
}
