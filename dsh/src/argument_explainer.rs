use crate::completion::command::CommandCompletionDatabase;
use crate::completion::context::ContextCorrector;
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

        let cursor = crate::completion::command_segment::char_to_byte(input, cursor);
        let parsed = self.parser.parse(input, cursor);
        let completion = self.db.get_command(&parsed.command)?;
        if !matches!(
            parsed.completion_context,
            CompletionContext::ShortOption | CompletionContext::LongOption
        ) && !parsed.current_token.starts_with('-')
        {
            return None;
        }
        let corrected = ContextCorrector::new(&self.db).correct_parsed_command_line(&parsed);
        if !matches!(
            corrected.completion_context,
            CompletionContext::ShortOption | CompletionContext::LongOption
        ) {
            return None;
        }
        let (word, offset) = self.parser.token_at_cursor(input, cursor)?;
        let short_flag;
        let name = if word.starts_with("--") {
            word.split('=').next()?
        } else if word.starts_with('-') && offset > 0 {
            // Explain the letter under the cursor in a short-flag cluster.
            short_flag = format!("-{}", word.chars().nth(offset)?);
            &short_flag
        } else {
            return None;
        };
        completion
            .options_for_path(&corrected.subcommand_path)
            .into_iter()
            .rev()
            .filter(|option| option.matches_name(name))
            .find_map(|option| option.description.clone())
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
                    multiple: false,
                    short: Some("-a".to_string()),
                    long: Some("--all".to_string()),
                    description: Some("Stage all modified files".to_string()),
                    inherit: None,
                    value_attached_only: false,
                    takes_value: false,
                    value_type: None,
                    argument: None,
                },
                CommandOption {
                    multiple: false,
                    short: Some("-m".to_string()),
                    long: Some("--message".to_string()),
                    description: Some("Commit message".to_string()),
                    inherit: None,
                    value_attached_only: false,
                    takes_value: false,
                    value_type: None,
                    argument: None,
                },
            ],
            arguments: vec![],
            subcommands: vec![],
        };

        let git = CommandCompletion {
            short_option_clusters: None,
            command: "git".to_string(),
            description: Some("Version control".to_string()),
            subcommands: vec![commit],
            global_options: vec![CommandOption {
                multiple: false,
                short: Some("-v".to_string()),
                long: Some("--version".to_string()),
                description: Some("Show version".to_string()),
                inherit: None,
                value_attached_only: false,
                takes_value: false,
                value_type: None,
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
    #[test]
    fn explanation_uses_character_cursor_in_active_command() {
        let explainer = ArgumentExplainer::with_db(make_git_db().into());
        let input = "echo 日本語 && git commit -a";
        assert_eq!(
            explainer.get_explanation(input, input.chars().count() - 1),
            Some("Stage all modified files".into())
        );
    }

    #[test]
    fn rustup_explains_the_option_in_its_cli_scope() {
        let mut db = CommandCompletionDatabase::new();
        db.add_command(
            serde_json::from_str(include_str!("../../completions/rustup.json")).unwrap(),
        );
        db.add_command(
            serde_json::from_str(include_str!("../../completions/git-lfs.json")).unwrap(),
        );
        let explainer = ArgumentExplainer::with_db(db.into());
        for (input, expected) in [
            ("rustup --quiet", Some("Quiet output")),
            (
                "rustup component list --quiet",
                Some("Use a single output column"),
            ),
            ("rustup -v component add --quiet", None),
            ("rustup show home --verbose", None),
            (
                "git-lfs migrate info --everything",
                Some("Migrate every local ref"),
            ),
        ] {
            assert_eq!(
                explainer.get_explanation(input, input.len() - 1).as_deref(),
                expected,
                "{input}"
            );
        }
    }
}
