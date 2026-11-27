//! The ghost-text path: a single best continuation for the line as typed,
//! which is why it only consults sources that are already cheap or cached and
//! never blocks the key handler on a refresh.
use super::*;
use crate::completion::generators::{option::OptionGenerator, subcommand::SubCommandGenerator};

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

        let parsed_command_line = self.convert_to_parsed_command_line(input, cursor_pos);
        let completion_dir = self.completion_directory(&parsed_command_line, current_dir)?;
        let request = CompletionRequest::new(input, &completion_dir, 10, cursor_pos);
        if parsed_command_line
            .source
            .as_ref()
            .is_some_and(|source| source.suppressed)
        {
            return None;
        }
        if let Some((reference, range)) =
            reference::Reference::at_cursor(&parsed_command_line, cursor_pos)
        {
            if range.end != cursor_pos {
                return None;
            }
            let typed = slice_chars(input, range.start, range.end);
            return reference
                .ghost_suffix(self, &typed)
                .map(|suffix| format!("{input}{suffix}"));
        }

        let replacement_range = completion_replacement_range(
            input,
            cursor_pos,
            &self.dynamic_provider_target(&parsed_command_line),
        )?;

        let mut candidates = self.collect_dynamic_candidates_cached(&request, &parsed_command_line);
        candidates.extend(
            self.collect_command_candidates_for_ghost(&parsed_command_line, request.current_dir),
        );

        let command_context = if parsed_command_line.command.is_empty() {
            None
        } else {
            Some(parsed_command_line.command.as_str())
        };

        let candidates = self.deduplicate_and_sort(candidates, 1, history, command_context);
        let candidate = candidates.first()?;
        let full = replace_char_range(
            input,
            replacement_range.start,
            replacement_range.end,
            &ghost_insertion(candidate, input, replacement_range),
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
        let target = self.dynamic_provider_target(parsed_command_line);
        let parsed_command_line = target.as_ref();
        if matches!(
            parsed_command_line.completion_context,
            parser::CompletionContext::Argument { .. }
                | parser::CompletionContext::OptionValue { .. }
        ) {
            return self.collect_safe_value_candidates_for_ghost(parsed_command_line, current_dir);
        }

        self.ensure_command_completion_loaded(&parsed_command_line.command);
        let db_lock = self.command_completion.lock();
        let Some(definition) = db_lock.get_command(&parsed_command_line.command) else {
            return Vec::new();
        };

        // The TAB generator can walk a directory or execute a Script when
        // subcommands and positional arguments share a slot. Ghost text only
        // needs the static names here. Values have their own cached-only path.
        let candidates = match parsed_command_line.completion_context {
            parser::CompletionContext::SubCommand => {
                SubCommandGenerator::generate_candidates(definition, parsed_command_line, |_, _| {
                    Ok(Vec::new())
                })
            }
            parser::CompletionContext::ShortOption => {
                OptionGenerator::generate_short_option_candidates(definition, parsed_command_line)
            }
            parser::CompletionContext::LongOption => {
                OptionGenerator::generate_long_option_candidates(definition, parsed_command_line)
            }
            _ => return Vec::new(),
        };
        match candidates {
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

        let (environment_names, system_command_ticket, filesystem) =
            self.runtime_completion_snapshot(current_dir);
        let generator = ArgumentGenerator::with_runtime_environment_ticket(
            &db_lock,
            &environment_names,
            Some(system_command_ticket),
        )
        .with_filesystem_context(Some(&filesystem));
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

fn ghost_insertion(
    candidate: &EnhancedCandidate,
    input: &str,
    range: CompletionReplacementRange,
) -> String {
    use skim::SkimItem;
    crate::completion::shell_path::format_candidates_for_range(
        vec![candidate.to_candidate()],
        input,
        range,
    )[0]
    .output()
    .to_string()
}
