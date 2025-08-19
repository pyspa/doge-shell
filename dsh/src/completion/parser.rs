#![allow(dead_code)]
use super::command::ArgumentType;
use regex::Regex;
use std::collections::VecDeque;

// Pre-compiled regex patterns for efficient option parsing
static SHORT_OPTION_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^-[a-zA-Z]$").unwrap());
static LONG_OPTION_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^--[a-zA-Z][a-zA-Z0-9-]{2,}$").unwrap());
static OPTION_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^-").unwrap());
static DOUBLE_DASH_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^--").unwrap());

/// Command line parsing result for dynamic completion
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCommandLine {
    /// Main command name
    pub command: String,
    /// Command arguments
    pub args: Vec<String>,
    /// Current argument being completed
    pub current_arg: Option<String>,
    /// Completion context
    pub completion_context: CompletionContext,
    /// Cursor index
    pub cursor_index: usize,
}

/// Command line parsing result
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCommand {
    /// Main command name
    pub command: String,
    /// Subcommand path (e.g., ["remote", "add"])
    pub subcommand_path: Vec<String>,
    /// Currently parsing token
    pub current_token: String,
    /// Completion context
    pub completion_context: CompletionContext,
    /// Already specified options
    pub specified_options: Vec<String>,
    /// Already specified arguments
    pub specified_arguments: Vec<String>,
}

/// Parameters for determining completion context
struct CompletionContextParams<'a> {
    cursor_token_index: usize,
    current_token: &'a str,
    subcommand_path: &'a [String],
    _specified_options: &'a [String],
    specified_arguments: &'a [String],
    all_tokens: &'a [String],
    has_space_after_command: bool,
}

/// Completion context (which part is currently being completed)
#[derive(Debug, Clone, PartialEq)]
pub enum CompletionContext {
    /// Complete command name
    Command,
    /// Complete subcommand
    SubCommand,
    /// Complete option (short form -x)
    ShortOption,
    /// Complete option (long form --xxx)
    LongOption,
    /// Complete option value
    OptionValue {
        option_name: String,
        value_type: Option<ArgumentType>,
    },
    /// Complete argument
    Argument {
        arg_index: usize,
        arg_type: Option<ArgumentType>,
    },
    /// Unknown (error state)
    Unknown,
}

/// Command line parser
pub struct CommandLineParser;

impl CommandLineParser {
    /// Create a new parser
    pub fn new() -> Self {
        Self
    }

    /// Parse command line
    pub fn parse(&self, input: &str, cursor_pos: usize) -> ParsedCommand {
        let tokens = self.tokenize(input);
        let cursor_token_index = self.find_cursor_token_index(&tokens, input, cursor_pos);

        self.analyze_tokens(tokens, cursor_token_index, input, cursor_pos)
    }

    /// Split input string into tokens
    fn tokenize(&self, input: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut current_token = String::new();
        let mut in_quotes = false;
        let mut quote_char = '"';

        for ch in input.chars() {
            match ch {
                '"' | '\'' if !in_quotes => {
                    in_quotes = true;
                    quote_char = ch;
                    current_token.push(ch);
                }
                ch if in_quotes && ch == quote_char => {
                    in_quotes = false;
                    current_token.push(ch);
                }
                ' ' | '\t' if !in_quotes => {
                    if !current_token.is_empty() {
                        tokens.push(current_token.clone());
                        current_token.clear();
                    }
                }
                _ => {
                    current_token.push(ch);
                }
            }
        }

        if !current_token.is_empty() {
            tokens.push(current_token);
        }

        tokens
    }

    /// Find token index corresponding to cursor position
    fn find_cursor_token_index(&self, tokens: &[String], input: &str, cursor_pos: usize) -> usize {
        let mut pos = 0;

        for (i, token) in tokens.iter().enumerate() {
            // Find token start position
            while pos < input.len() && input.chars().nth(pos).unwrap().is_whitespace() {
                pos += 1;
            }

            let token_start = pos;
            let token_end = pos + token.len();

            if cursor_pos >= token_start && cursor_pos <= token_end {
                return i;
            }

            pos = token_end;
        }

        tokens.len() // When cursor is at the end
    }

    /// Analyze tokens to determine completion context
    fn analyze_tokens(
        &self,
        tokens: Vec<String>,
        cursor_token_index: usize,
        input: &str,
        _cursor_pos: usize,
    ) -> ParsedCommand {
        if tokens.is_empty() {
            return ParsedCommand {
                command: String::new(),
                subcommand_path: Vec::new(),
                current_token: String::new(),
                completion_context: CompletionContext::Command,
                specified_options: Vec::new(),
                specified_arguments: Vec::new(),
            };
        }

        let command = tokens[0].clone();
        let mut subcommand_path = Vec::new();
        let mut specified_options = Vec::new();
        let mut specified_arguments = Vec::new();
        let mut tokens_queue: VecDeque<String> = tokens.into_iter().skip(1).collect();

        // Check if there's a space after the command
        let has_space_after_command = self.has_space_after_command(input, &command);

        // Parse subcommands
        let mut subcommand_count = 0;
        while let Some(token) = tokens_queue.front() {
            if OPTION_REGEX.is_match(token) {
                break; // End subcommand parsing when options start
            }

            // Determine if it's an argument or subcommand (simplified version)
            // Only treat the first few tokens as subcommands
            if subcommand_count < 2 && self.looks_like_subcommand(token) {
                subcommand_path.push(tokens_queue.pop_front().unwrap());
                subcommand_count += 1;
            } else {
                break; // End subcommand parsing when arguments start
            }
        }

        // Parse options and arguments
        let mut skip_next = false;
        for (i, token) in tokens_queue.iter().enumerate() {
            if skip_next {
                skip_next = false;
                continue;
            }

            if OPTION_REGEX.is_match(token) {
                specified_options.push(token.clone());

                // Check if next token is option value
                if let Some(next_token) = tokens_queue.get(i + 1)
                    && !OPTION_REGEX.is_match(next_token) && self.option_takes_value(token) {
                        skip_next = true;
                    }
            } else {
                specified_arguments.push(token.clone());
            }
        }

        // Determine current token and completion context
        let (current_token, completion_context) = if cursor_token_index == 0 {
            (command.clone(), CompletionContext::Command)
        } else {
            let all_tokens: Vec<String> = std::iter::once(command.clone())
                .chain(subcommand_path.iter().cloned())
                .chain(tokens_queue.iter().cloned())
                .collect();

            let current_token = if cursor_token_index < all_tokens.len() {
                all_tokens[cursor_token_index].clone()
            } else {
                String::new()
            };

            let context = self.determine_completion_context(CompletionContextParams {
                cursor_token_index,
                current_token: &current_token,
                subcommand_path: &subcommand_path,
                _specified_options: &specified_options,
                specified_arguments: &specified_arguments,
                all_tokens: &all_tokens,
                has_space_after_command,
            });

            (current_token, context)
        };

        ParsedCommand {
            command,
            subcommand_path,
            current_token,
            completion_context,
            specified_options,
            specified_arguments,
        }
    }

    /// Check if there's a space after the command
    fn has_space_after_command(&self, input: &str, command: &str) -> bool {
        if let Some(command_end_pos) = input.find(command) {
            let after_command_pos = command_end_pos + command.len();
            if after_command_pos < input.len() {
                let char_after_command = input.chars().nth(after_command_pos);
                return char_after_command.is_some_and(|c| c.is_whitespace());
            }
        }
        false
    }

    /// Determine if token looks like a subcommand
    fn looks_like_subcommand(&self, token: &str) -> bool {
        // Simple determination: not an option and not a file path
        if OPTION_REGEX.is_match(token) {
            return false;
        }

        // Consider as file if it has file extension
        if token.contains('.')
            && token
                .rfind('.')
                .is_some_and(|i| i > 0 && i < token.len() - 1)
        {
            return false;
        }

        // Consider as file path if it has path separator
        if token.contains('/') || token.contains('\\') {
            return false;
        }

        // Common subcommand name patterns (strict version)
        let common_subcommands = [
            "add",
            "remove",
            "delete",
            "create",
            "update",
            "get",
            "set",
            "list",
            "show",
            "start",
            "stop",
            "restart",
            "status",
            "config",
            "init",
            "clone",
            "pull",
            "push",
            "commit",
            "branch",
            "checkout",
            "merge",
            "rebase",
            "tag",
            "log",
            "diff",
            "build",
            "run",
            "test",
            "install",
            "uninstall",
            "upgrade",
            "clean",
            "check",
            "remote",
            "fetch",
            "reset",
            "stash",
            "cherry-pick",
            "revert",
            "blame",
            "bisect",
            "new",
            "publish",
            "search",
            "doc",
            "fmt",
            "clippy",
            "bench",
            "update",
            "tree",
            "login",
            "logout",
            "whoami",
            "owner",
            "yank",
            "verify-project",
        ];

        // Only allow known subcommands (more strict)
        common_subcommands.contains(&token)
    }

    /// Determine completion context
    fn determine_completion_context(&self, params: CompletionContextParams) -> CompletionContext {
        if params.cursor_token_index == 0 {
            return CompletionContext::Command;
        }

        // If current token is an option
        if DOUBLE_DASH_REGEX.is_match(params.current_token) {
            return CompletionContext::LongOption;
        } else if OPTION_REGEX.is_match(params.current_token) {
            // Support both short options (-x) and long options starting with single dash (-xxx)
            if params.current_token.len() == 2 {
                return CompletionContext::ShortOption;
            } else {
                return CompletionContext::LongOption;
            }
        }

        // If previous token is an option that takes a value
        if params.cursor_token_index > 0 {
            let prev_token = &params.all_tokens[params.cursor_token_index - 1];
            if OPTION_REGEX.is_match(prev_token) && self.option_takes_value(prev_token) {
                return CompletionContext::OptionValue {
                    option_name: prev_token.clone(),
                    value_type: None, // In actual implementation, get from completion data
                };
            }
        }

        // Subcommand completion only if there's a space after the command
        if params.cursor_token_index == 1 && !params.has_space_after_command {
            // If we're at the first position after command but there's no space,
            // treat it as command completion (not subcommand)
            return CompletionContext::Command;
        }

        // Subcommand or argument
        if params.subcommand_path.is_empty() || self.looks_like_subcommand(params.current_token) {
            // Only allow subcommand completion if there's a space after the command
            if params.has_space_after_command {
                CompletionContext::SubCommand
            } else {
                CompletionContext::Command
            }
        } else {
            // If current token is an argument, calculate its index
            // Don't include current token (since it's the completion target)
            let arg_index = params.specified_arguments.len().saturating_sub(
                if params
                    .specified_arguments
                    .contains(&params.current_token.to_string())
                {
                    1
                } else {
                    0
                },
            );
            CompletionContext::Argument {
                arg_index,
                arg_type: None, // In actual implementation, get from completion data
            }
        }
    }

    /// Determine if option takes a value (simplified version)
    fn option_takes_value(&self, option: &str) -> bool {
        // Options that commonly take values
        matches!(
            option,
            "--message" | "-m" | "--target" | "--features" | "--git" | "--path" | "--name"
        )
    }
}

impl Default for CommandLineParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_simple() {
        let parser = CommandLineParser::new();
        let tokens = parser.tokenize("git add file.txt");
        assert_eq!(tokens, vec!["git", "add", "file.txt"]);
    }

    #[test]
    fn test_tokenize_with_quotes() {
        let parser = CommandLineParser::new();
        let tokens = parser.tokenize("git commit -m \"test message\"");
        assert_eq!(tokens, vec!["git", "commit", "-m", "\"test message\""]);
    }

    #[test]
    fn test_parse_command_only() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git", 3);
        assert_eq!(result.command, "git");
        assert_eq!(result.completion_context, CompletionContext::Command);
    }

    #[test]
    fn test_parse_command_with_space() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git ", 4);
        assert_eq!(result.command, "git");
        assert_eq!(result.completion_context, CompletionContext::SubCommand);
    }

    #[test]
    fn test_parse_command_without_space() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git", 3);
        assert_eq!(result.command, "git");
        assert_eq!(result.completion_context, CompletionContext::Command);
    }

    #[test]
    fn test_parse_subcommand_with_space() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git add", 7);
        assert_eq!(result.command, "git");
        assert_eq!(result.subcommand_path, vec!["add"]);
        assert_eq!(result.completion_context, CompletionContext::SubCommand);
    }

    #[test]
    fn test_has_space_after_command() {
        let parser = CommandLineParser::new();
        assert!(parser.has_space_after_command("git ", "git"));
        assert!(parser.has_space_after_command("git add", "git"));
        assert!(!parser.has_space_after_command("git", "git"));
        assert!(!parser.has_space_after_command("gitadd", "git"));
    }

    #[test]
    fn test_parse_nested_subcommand() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git remote add", 14);

        assert_eq!(result.command, "git");
        assert_eq!(result.subcommand_path, vec!["remote", "add"]);
        assert_eq!(result.completion_context, CompletionContext::SubCommand);
    }

    #[test]
    fn test_parse_long_option() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git commit --message", 20);

        assert_eq!(result.command, "git");
        assert_eq!(result.subcommand_path, vec!["commit"]);
        assert_eq!(result.completion_context, CompletionContext::LongOption);
        assert!(result.specified_options.contains(&"--message".to_string()));
    }

    #[test]
    fn test_parse_short_option() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git commit -m", 13);

        assert_eq!(result.command, "git");
        assert_eq!(result.subcommand_path, vec!["commit"]);
        assert_eq!(result.completion_context, CompletionContext::ShortOption);
    }

    #[test]
    fn test_parse_single_dash_long_option() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git commit -message", 19);

        assert_eq!(result.command, "git");
        assert_eq!(result.subcommand_path, vec!["commit"]);
        assert_eq!(result.completion_context, CompletionContext::LongOption);
        assert!(result.specified_options.contains(&"-message".to_string()));
    }

    #[test]
    fn test_parse_option_value() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git commit -m \"test", 19);

        assert_eq!(result.command, "git");
        assert_eq!(result.subcommand_path, vec!["commit"]);
        if let CompletionContext::OptionValue { option_name, .. } = result.completion_context {
            assert_eq!(option_name, "-m");
        } else {
            panic!("Expected OptionValue context");
        }
    }

    #[test]
    fn test_parse_argument() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git add file", 12);

        assert_eq!(result.command, "git");
        assert_eq!(result.subcommand_path, vec!["add"]);
        if let CompletionContext::Argument { arg_index, .. } = result.completion_context {
            assert_eq!(arg_index, 0);
        } else {
            panic!(
                "Expected Argument context, got: {:?}",
                result.completion_context
            );
        }
    }

    #[test]
    fn test_parse_double_dash_option() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git add --", 10);

        assert_eq!(result.command, "git");
        assert_eq!(result.subcommand_path, vec!["add"]);
        assert_eq!(result.current_token, "--");
        assert_eq!(result.completion_context, CompletionContext::LongOption);
    }

    #[test]
    fn test_space_detection_edge_cases() {
        let parser = CommandLineParser::new();

        // Test with tab character
        let result = parser.parse("git\t", 4);
        assert_eq!(result.completion_context, CompletionContext::SubCommand);

        // Test with multiple spaces
        let result = parser.parse("git   ", 6);
        assert_eq!(result.completion_context, CompletionContext::SubCommand);

        // Test cursor at different positions
        let result = parser.parse("git ", 3); // cursor at end of command
        assert_eq!(result.completion_context, CompletionContext::Command);

        let result = parser.parse("git ", 4); // cursor at space
        assert_eq!(result.completion_context, CompletionContext::SubCommand);
    }

    #[test]
    fn test_subcommand_completion_requires_space() {
        let parser = CommandLineParser::new();

        // Without space - should be command completion
        let result = parser.parse("git", 3);
        assert_eq!(result.completion_context, CompletionContext::Command);

        // With space - should be subcommand completion
        let result = parser.parse("git ", 4);
        assert_eq!(result.completion_context, CompletionContext::SubCommand);

        // Partial subcommand without space after command - should be command completion
        let result = parser.parse("gita", 4);
        assert_eq!(result.completion_context, CompletionContext::Command);

        // Partial subcommand with space after command - should be subcommand completion
        let result = parser.parse("git a", 5);
        assert_eq!(result.completion_context, CompletionContext::SubCommand);
    }
}
