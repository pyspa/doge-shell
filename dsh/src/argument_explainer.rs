use crate::completion::command::{CommandCompletion, CommandCompletionDatabase, SubCommand};
use crate::completion::json_loader::JsonCompletionLoader;
use crate::completion::parser::{CommandLineParser, CompletionContext};

pub struct ArgumentExplainer {
    db: CommandCompletionDatabase,
    parser: CommandLineParser,
}

impl Default for ArgumentExplainer {
    fn default() -> Self {
        Self::new()
    }
}

impl ArgumentExplainer {
    pub fn new() -> Self {
        let loader = JsonCompletionLoader::new();
        let db = loader.load_database().unwrap_or_default();
        Self {
            db,
            parser: CommandLineParser::new(),
        }
    }

    pub fn with_db(db: CommandCompletionDatabase) -> Self {
        Self {
            db,
            parser: CommandLineParser::new(),
        }
    }

    pub fn get_explanation(&self, input: &str, cursor: usize) -> Option<String> {
        if input.trim().is_empty() {
            return None;
        }

        let parsed = self.parser.parse(input, cursor);
        let cmd_name = &parsed.command;

        // Lookup command
        let completion = self.db.get_command(cmd_name)?;

        // Resolve subcommand chain
        let chain = self.resolve_command_chain(completion, &parsed.subcommand_path);

        // Based on context, decide what to explain
        match &parsed.completion_context {
            CompletionContext::ShortOption | CompletionContext::LongOption => {
                // Determine the option token
                let _token = &parsed.current_token;
                // For ShortOption, we might have combined flags "-am".
                // CommandLineParser gives us the full token as current_token.
                // But we need to know WHICH char the cursor is on if it's combined.
                // ParsedCommandLine doesn't give sub-token cursor offset directly for short flags.
                // However, we have `cursor` (input index).
                // dsh::completion::parser doesn't expose span info publicly in ParsedCommandLine struct.
                // But we can approximate relative offset in the token.

                // If it's a short option group like -am, and cursor is on 'm'.
                // current_token is "-am".
                // We need to calculate offset.
                // Let's fallback to calculating offset manually since we have the token and cursor.

                // If token is "-am", and cursor is at end, offset is 3 (len).
                // If we are strictly matching existing options, usually cursor is ON the character or just after.

                // Let's rely on `explain_option`'s logic if we pass the token and approximate offset.
                // But `explain_option` took `cursor_offset`.
                // We don't have exact offset from parsed result.
                // Re-calculating:
                // Find token in input? It might appear multiple times.
                // `CommandLineParser` uses spans internally but drops them.
                // That's a pity.
                // But we can use `parsed.cursor_index`? That is just `cursor`.
                // Wait, `parsed` has `current_token` but not its start index.

                // Workaround: Re-locate `parsed.current_token` near `cursor`?
                // Or just use `analyze_cursor_context` style logic ONLY for finding offset,
                // but trust `parsed` for context?

                // Actually, if we use `parsed.completion_context` = `ShortOption`,
                // `parsed.current_token` is the flag (e.g. "-a").
                // If we have "-am", context is `ShortOption`?
                // Let's verify parser behavior.
                // If input is "git commit -am", cursor at 'm'.
                // Token is "-am". Context: ShortOption (len=3? No, len=2 check in parser).
                // Parser says: if len==2 -> ShortOption, else LongOption?
                // Lines 446-450 of parser.rs:
                // if params.current_token.len() == 2 { ShortOption } else { LongOption }
                // So "-am" (len 3) is treated as `LongOption` in `determine_completion_context`.
                // This seems like a parser simplification/bug for combined short options.
                // But for "explanation", we treat it as valid option text.

                // So if context is LongOption, it might be "--message" OR "-am".
                // We need to handle both.

                // We still need the offset to distinguish 'a' vs 'm' in "-am".
                // Since we don't have start index, we can scan backward from cursor to find whitespace?
                // This is risky with quotes.

                // Alternative: Use `parsed.current_token` logic but implementing basic offset calculation.
                // If `current_token` aligns with text ending at `cursor` (or near).
                // Let's optimistically assume cursor is at the end of the token string matching `current_token`.
                // Wait, cursor can be in middle.

                // Let's scan backward from cursor to find the start of `current_token`.
                let token = &parsed.current_token;
                let _token_len = token.len();

                // Verify if input[cursor-len..cursor] == token?
                // No, cursor can be in middle.
                // Scan left from cursor for `token` start.
                // This is getting parsing-heavy again.
                // Maybe our `analyze_cursor_context` WAS useful for offset?
                // But it failed on quotes.

                // Let's combine `parsed` (for reliable context/chain) with `analyze_cursor_context` (for offset)?
                // Or implementing a helper that finds "word bounding cursor" respecting quotes?

                // Let's try to infer offset by looking at `parsed.current_token`.
                // If we are inside `ShortOption` or `LongOption` context, we are likely on a flag.
                // We need to explain `token`.
                // If it starts with `--`, checking the whole name is enough.
                // If it starts with `-` (and not `--`), we check char at relative position.

                // How to get relative position?
                // Use a simplified tokenizer just to find start of current word around cursor?
                // parser.tokenize_with_positions is private.
                // Copy-paste simplified logic or make it public?
                // I cannot modify parser.rs easily without risking breaks (it's core).
                // Copying the `tokenize_with_positions` logic to here seems safest and robust.

                let (words, index, offset) =
                    self.analyze_cursor_context_with_quotes(input, cursor)?;
                // Verify that `words[index]` matches `parsed.current_token`?
                // Should match mostly.
                if let Some(w) = words.get(index) {
                    return self.explain_option(&chain, w, offset);
                }
            }
            _ => {}
        }

        None
    }

    fn analyze_cursor_context_with_quotes(
        &self,
        input: &str,
        cursor: usize,
    ) -> Option<(Vec<String>, usize, usize)> {
        // Simplified version of parser logic to get indices
        // We only need to find the word under cursor and its start position.

        let mut words = Vec::new();
        let _word_start = 0;
        let mut in_quote = false;
        let mut quote_char = ' ';
        let mut word_idx = 0;
        let mut target_index = 0;
        let mut target_offset = 0;
        let mut found = false;

        // Iterator with indices
        let indices = input.char_indices().peekable();
        let mut current_word = String::new();
        let mut current_word_start = 0;
        let mut in_word = false;

        for (i, c) in indices {
            if in_quote {
                if c == quote_char {
                    in_quote = false;
                }
                current_word.push(c);
            } else if c == '"' || c == '\'' {
                in_quote = true;
                quote_char = c;
                if !in_word {
                    in_word = true;
                    current_word_start = i;
                }
                current_word.push(c);
            } else if c.is_whitespace() {
                if in_word {
                    words.push(current_word.clone());
                    // Check if cursor was in this word (inclusive of end of word)
                    // If cursor is at `i`, it is exactly at the end of this word.
                    if !found && cursor >= current_word_start && cursor <= i {
                        target_index = word_idx;
                        target_offset = cursor - current_word_start;
                        found = true;
                    }

                    word_idx += 1;
                    current_word.clear();
                    in_word = false;
                }
                // whitespace, continue
            } else {
                if !in_word {
                    in_word = true;
                    current_word_start = i;
                }
                current_word.push(c);
            }
        }

        if in_word {
            words.push(current_word);
            let end = input.len();
            if !found
                && cursor >= current_word_start && cursor <= end {
                    target_index = word_idx;
                    target_offset = cursor - current_word_start;
                    found = true;
                }
        }

        if found {
            Some((words, target_index, target_offset))
        } else {
            None
        }
    }

    // Collects the hierarchy: [RootCommand, SubCommand1, SubCommand2]
    // We treat RootCommand's global_options as available everywhere.
    // Each SubCommand's options are available in its scope.
    fn resolve_command_chain<'a>(
        &self,
        root: &'a CommandCompletion,
        subcommand_path: &[String],
    ) -> Vec<CompletionContextRef<'a>> {
        let mut chain = Vec::new();
        chain.push(CompletionContextRef::Root(root));

        let mut current_level_subcommands = &root.subcommands;

        for sub_name in subcommand_path {
            if let Some(sub) = current_level_subcommands
                .iter()
                .find(|s| s.name == *sub_name)
            {
                chain.push(CompletionContextRef::Sub(sub));
                current_level_subcommands = &sub.subcommands;
            }
        }
        chain
    }

    fn explain_option(
        &self,
        chain: &[CompletionContextRef],
        word: &str,
        cursor_offset: usize,
    ) -> Option<String> {
        if word.starts_with("--") {
            let opt_name = word.split('=').next().unwrap_or(word);
            for ctx in chain.iter().rev() {
                if let Some(desc) = ctx.find_long_option(opt_name) {
                    return Some(desc.to_string());
                }
            }
        } else if word.starts_with('-') {
            if cursor_offset == 0 {
                return None;
            }
            // If word is something like "-m" and cursor_offset is 1 ('m'), char is 'm'.
            // If word is "-am" and cursor_offset is 2 ('m'), char is 'm'.
            // Handle quotes? If word is "-m"foo"", and cursor is at 'foo', offset > 2.
            // If offset >= len, it's out of bounds?
            let char_at_cursor = word.chars().nth(cursor_offset)?;

            // If char is alphanumeric, it might be a flag.
            // Check if we are inside a value?
            // E.g. -m"foo". If cursor on f, it's not a short flag.
            // How do we differentiate "-am" (two flags) vs "-mfoo" (flag + value)?
            // It depends if 'a' takes a value.
            // This requires scanning from left and checking if option takes value.
            // Complex.
            // For MVP: assume standard combined short flags without value, OR assume user treats them as simple flags.
            // If user hovers 'f' in '-xzvf', show 'file'.
            // If user hovers 'o' in '-mfoo', it shouldn't show matching for '-o'.
            // To fix this, we need to know if previous char consumed a value.

            // Simplification: Check exact match for char as short option.
            // If I verify that the char corresponds to a defined short option, show it.
            // False positive: '-mfoo' where 'f' is defined as '-f'.
            // Risk is acceptable for inline explainer (user logic check: "Wait, f is not a flag here?").
            // Or we check if PREDECESSORS consume value?

            let short_flag = format!("-{}", char_at_cursor);

            for ctx in chain.iter().rev() {
                if let Some(desc) = ctx.find_short_option(&short_flag) {
                    return Some(desc.to_string());
                }
            }
        }
        None
    }
}

enum CompletionContextRef<'a> {
    Root(&'a CommandCompletion),
    Sub(&'a SubCommand),
}

impl<'a> CompletionContextRef<'a> {
    fn find_long_option(&self, name: &str) -> Option<&str> {
        let options = match self {
            CompletionContextRef::Root(c) => &c.global_options,
            CompletionContextRef::Sub(s) => &s.options,
        };
        options
            .iter()
            .find(|o| o.long.as_deref() == Some(name))
            .and_then(|o| o.description.as_deref())
    }

    fn find_short_option(&self, name: &str) -> Option<&str> {
        let options = match self {
            CompletionContextRef::Root(c) => &c.global_options,
            CompletionContextRef::Sub(s) => &s.options,
        };
        options
            .iter()
            .find(|o| o.short.as_deref() == Some(name))
            .and_then(|o| o.description.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::command::{CommandCompletion, CommandOption, SubCommand};

    fn make_git_db() -> CommandCompletionDatabase {
        let mut db = CommandCompletionDatabase::new();
        let commit = SubCommand {
            name: "commit".to_string(),
            description: Some("Record changes".to_string()),
            options: vec![
                CommandOption {
                    short: Some("-a".to_string()),
                    long: Some("--all".to_string()),
                    description: Some("Stage all modified files".to_string()),
                },
                CommandOption {
                    short: Some("-m".to_string()),
                    long: Some("--message".to_string()),
                    description: Some("Commit message".to_string()),
                },
            ],
            arguments: vec![],
            subcommands: vec![],
        };

        let git = CommandCompletion {
            command: "git".to_string(),
            description: Some("Version control".to_string()),
            subcommands: vec![commit],
            global_options: vec![CommandOption {
                short: Some("-v".to_string()),
                long: Some("--version".to_string()),
                description: Some("Show version".to_string()),
            }],
            arguments: vec![],
        };
        db.add_command(git);
        db
    }

    #[test]
    fn test_explain_git_commit_short() {
        let db = make_git_db();
        let explainer = ArgumentExplainer::with_db(db);

        let input = "git commit -a";
        let cursor = 12; // 'a'
        let expl = explainer.get_explanation(input, cursor);
        assert_eq!(expl, Some("Stage all modified files".to_string()));
    }

    #[test]
    fn test_explain_git_commit_combined() {
        let db = make_git_db();
        let explainer = ArgumentExplainer::with_db(db);

        let input = "git commit -am";
        // -:11, a:12, m:13
        let cursor_m = 13;
        let expl_m = explainer.get_explanation(input, cursor_m);
        assert_eq!(expl_m, Some("Commit message".to_string()));

        let cursor_a = 12;
        let expl_a = explainer.get_explanation(input, cursor_a);
        assert_eq!(expl_a, Some("Stage all modified files".to_string()));
    }

    #[test]
    fn test_explain_global_option() {
        let db = make_git_db();
        let explainer = ArgumentExplainer::with_db(db);

        let input = "git -v";
        let cursor = 5; // on 'v'
        let expl = explainer.get_explanation(input, cursor);
        assert_eq!(expl, Some("Show version".to_string()));
    }

    #[test]
    fn test_explain_with_quotes() {
        let db = make_git_db();
        let explainer = ArgumentExplainer::with_db(db);

        // git commit -m "msg"
        // 0123456789012345678
        // git commit -m "msg"
        // m is at 12
        let input = "git commit -m \"msg\"";
        let cursor = 12;
        let expl = explainer.get_explanation(input, cursor);
        assert_eq!(expl, Some("Commit message".to_string()));
    }
}
