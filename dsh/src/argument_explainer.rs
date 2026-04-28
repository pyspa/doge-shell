use crate::completion::command::{CommandCompletion, CommandCompletionDatabase, SubCommand};
use crate::completion::json_loader::JsonCompletionLoader;
use crate::completion::parser::{CommandLineParser, CompletionContext};

use std::sync::Arc;

pub struct ArgumentExplainer {
    db: Arc<CommandCompletionDatabase>,
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

    pub fn with_db(db: Arc<CommandCompletionDatabase>) -> Self {
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

        match &parsed.completion_context {
            CompletionContext::ShortOption | CompletionContext::LongOption => {
                let (word, offset) = self.parser.token_at_cursor(input, cursor)?;
                return self.explain_option(&chain, &word, offset);
            }
            _ => {}
        }

        None
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
            aliases: vec![],
            options: vec![
                CommandOption {
                    short: Some("-a".to_string()),
                    long: Some("--all".to_string()),
                    description: Some("Stage all modified files".to_string()),
                    argument: None,
                },
                CommandOption {
                    short: Some("-m".to_string()),
                    long: Some("--message".to_string()),
                    description: Some("Commit message".to_string()),
                    argument: None,
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
                argument: None,
            }],
            arguments: vec![],
        };
        db.add_command(git);
        db
    }

    #[test]
    fn test_explain_git_commit_short() {
        let db = make_git_db();
        let explainer = ArgumentExplainer::with_db(db.into());

        let input = "git commit -a";
        let cursor = 12; // 'a'
        let expl = explainer.get_explanation(input, cursor);
        assert_eq!(expl, Some("Stage all modified files".to_string()));
    }

    #[test]
    fn test_explain_git_commit_combined() {
        let db = make_git_db();
        let explainer = ArgumentExplainer::with_db(db.into());

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
        let explainer = ArgumentExplainer::with_db(db.into());

        let input = "git -v";
        let cursor = 5; // on 'v'
        let expl = explainer.get_explanation(input, cursor);
        assert_eq!(expl, Some("Show version".to_string()));
    }

    #[test]
    fn test_explain_with_quotes() {
        let db = make_git_db();
        let explainer = ArgumentExplainer::with_db(db.into());

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
