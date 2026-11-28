//! Git's global `-C` options select the cwd shared by TAB, cached ghost
//! providers and filesystem candidates. Resolve source words without executing
//! shell syntax, and stop at the cursor or the first subcommand.
use super::*;
use crate::completion::shell_path::decode_literal_word;

impl IntegratedCompletionEngine {
    /// Share TAB's lookup context with the REPL's cheap path-only fallback.
    pub(crate) fn filesystem_context_for_input<'a>(
        &self,
        input: &'a str,
        cursor: usize,
        current_dir: &Path,
    ) -> Option<(FileSystemContext, GenericCompletion, &'a str)> {
        let parsed = self.convert_to_parsed_command_line(input, cursor);
        if parsed
            .source
            .as_ref()
            .is_some_and(|source| source.suppressed)
        {
            return None;
        }
        let directory = self.completion_directory(&parsed, current_dir)?;
        let target = self.dynamic_provider_target(&parsed);
        let range = completion_replacement_range(input, cursor, &target)?;
        let start = super::super::command_segment::char_to_byte(input, range.start);
        let end = super::super::command_segment::char_to_byte(input, range.end);
        Some((
            self.filesystem_context(&directory),
            self.generic_completion(&target),
            &input[start..end],
        ))
    }

    pub(crate) fn generic_completion_at(&self, input: &str, cursor: usize) -> GenericCompletion {
        let parsed = self.convert_to_parsed_command_line(input, cursor);
        self.generic_completion(&self.dynamic_provider_target(&parsed))
    }

    pub(super) fn completion_directory<'a>(
        &self,
        parsed: &ParsedCommandLine,
        current_dir: &'a Path,
    ) -> Option<Cow<'a, Path>> {
        let target = self.dynamic_provider_target(parsed);
        if target.command != "git" {
            return Some(Cow::Borrowed(current_dir));
        }
        let Some(source) = &target.source else {
            return Some(Cow::Borrowed(current_dir));
        };
        // Redirection is performed by the shell, before Git changes cwd.
        if source.cursor_follows_redirect() {
            return Some(Cow::Borrowed(current_dir));
        }
        let db = self.command_completion.lock();
        let Some(definition) = db.get_command("git") else {
            return Some(Cow::Borrowed(current_dir));
        };
        let mut words = source
            .argv()
            .take_while(|(i, _)| *i < source.cursor_word)
            .map(|(_, word)| word);
        let mut directory = Cow::Borrowed(current_dir);
        while let Some(word) = words.next() {
            let option = decode_literal_word(word);
            if let Some(attached_path) = option.strip_prefix("-C") {
                let (path, home_expansion) = if attached_path.is_empty() {
                    let Some(value) = words.next() else { break };
                    (
                        decode_literal_word(value),
                        value == "~" || value.starts_with("~/"),
                    )
                } else {
                    (attached_path.to_string(), false)
                };
                // `-C ''` intentionally leaves the current directory alone.
                if !path.is_empty() {
                    let next = self
                        .filesystem_context(&directory)
                        .lookup_directory(&path, home_expansion);
                    if !next.is_dir() {
                        return None;
                    }
                    directory = Cow::Owned(next);
                }
            } else if let Some(option) = definition
                .global_options
                .iter()
                .find(|o| o.matches_name(&option))
            {
                if option.takes_separate_value() {
                    words.next();
                }
            } else if option.starts_with('-') {
                // Attached/inline values occupy one word. Never interpret a
                // `-C` inside a config value as another directory option.
                continue;
            } else {
                break;
            }
        }
        Some(directory)
    }
}
