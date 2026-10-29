//! Git's global `-C` options select the cwd shared by TAB, cached ghost
//! providers and filesystem candidates. Resolve source words without executing
//! shell syntax, and stop at the cursor or the first subcommand.
use super::*;
use crate::completion::shell_path::decode_literal_word;

impl IntegratedCompletionEngine {
    /// Share TAB's lookup context with the REPL's cheap path-only fallback.
    pub(crate) fn filesystem_context_for_input(
        &self,
        input: &str,
        cursor: usize,
        current_dir: &Path,
    ) -> Option<FileSystemContext> {
        let parsed = self.convert_to_parsed_command_line(input, cursor);
        if parsed
            .source
            .as_ref()
            .is_some_and(|source| source.suppressed)
        {
            return None;
        }
        let directory = self.completion_directory(&parsed, current_dir)?;
        Some(self.filesystem_context(&directory))
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
        if source
            .cursor_word
            .checked_sub(1)
            .and_then(|index| source.words.get(index))
            .is_some_and(|word| parser::CommandLineParser::is_redirect_operator(&word.raw))
        {
            return Some(Cow::Borrowed(current_dir));
        }
        let db = self.command_completion.lock();
        let Some(definition) = db.get_command("git") else {
            return Some(Cow::Borrowed(current_dir));
        };
        let mut redirect_target = false;
        let mut words = source.words[..source.cursor_word]
            .iter()
            .skip(1)
            .filter(|word| {
                if redirect_target {
                    redirect_target = false;
                    false
                } else if parser::CommandLineParser::is_redirect_operator(&word.raw) {
                    redirect_target = true;
                    false
                } else {
                    true
                }
            });
        let mut directory = Cow::Borrowed(current_dir);
        while let Some(word) = words.next() {
            let option = decode_literal_word(&word.raw);
            if let Some(attached_path) = option.strip_prefix("-C") {
                let (path, home_expansion) = if attached_path.is_empty() {
                    let Some(value) = words.next() else { break };
                    (
                        decode_literal_word(&value.raw),
                        value.raw == "~" || value.raw.starts_with("~/"),
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
                if option.expects_value() {
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
