use super::command::ArgumentType;
use super::shell_token::{self, SeparatorMode, ShellTokenSpan};
use std::collections::VecDeque;

#[inline]
fn is_option_token(token: &str) -> bool {
    token.starts_with('-')
}

#[inline]
fn is_long_option_token(token: &str) -> bool {
    token.starts_with("--")
}

pub(crate) fn split_inline_long_option(token: &str) -> Option<(&str, &str)> {
    if !is_long_option_token(token) {
        return None;
    }

    let (name, value) = token.split_once('=')?;
    if name.len() <= 2 {
        return None;
    }

    Some((name, value))
}

/// Command line parsing result for dynamic completion
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCommandLine {
    /// Main command name
    pub command: String,
    /// Subcommand path (e.g., ["remote", "add"])
    pub subcommand_path: Vec<String>,
    /// Raw tokens after command/subcommand (preserving order)
    pub raw_args: Vec<String>,
    /// Command arguments
    pub args: Vec<String>,
    /// Command  options
    pub options: Vec<String>,
    /// Currently parsing token
    pub current_token: String,
    /// Current argument being completed
    pub current_arg: Option<String>,
    /// Completion context
    pub completion_context: CompletionContext,
    /// Already specified options
    pub specified_options: Vec<String>,
    /// Already specified arguments
    pub specified_arguments: Vec<String>,
    /// Cursor index
    pub cursor_index: usize,
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
    after_end_of_options: bool,
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
    pub fn parse(&self, input: &str, cursor_pos: usize) -> ParsedCommandLine {
        let spans = self.tokenize_with_positions(input);
        let mut tokens: Vec<String> = spans.iter().map(|s| s.raw.clone()).collect();
        let (cursor_token_index, is_inside_token) =
            self.find_cursor_token_index(&spans, cursor_pos);

        // Adjust tokens based on cursor position
        if is_inside_token {
            if cursor_token_index < tokens.len() {
                let span = &spans[cursor_token_index];
                let relative_pos = cursor_pos.saturating_sub(span.byte_start);
                if relative_pos < span.raw.len() {
                    // Start from relative_pos and backtrack to find a valid char boundary
                    // This handles cases where cursor is in the middle of a multibyte char
                    let mut safe_pos = relative_pos;
                    while !span.raw.is_char_boundary(safe_pos) && safe_pos > 0 {
                        safe_pos -= 1;
                    }
                    tokens[cursor_token_index] = span.raw[..safe_pos].to_string();
                }
            }
        } else {
            // Insert empty token at cursor position (gap)
            if cursor_token_index <= tokens.len() {
                tokens.insert(cursor_token_index, String::new());
            }
        }

        self.analyze_tokens(tokens, cursor_token_index, input, cursor_pos)
    }

    /// Split input string into tokens
    // Currently exercised only by unit tests; retained as the simple
    // positions-free counterpart of `tokenize_with_positions`.
    #[allow(dead_code)]
    fn tokenize(&self, input: &str) -> Vec<String> {
        self.tokenize_with_positions(input)
            .into_iter()
            .map(|s| s.raw)
            .collect()
    }

    /// Return the complete token under the cursor and the cursor offset inside it.
    pub(crate) fn token_at_cursor(
        &self,
        input: &str,
        cursor_pos: usize,
    ) -> Option<(String, usize)> {
        let spans = self.tokenize_with_positions(input);
        let (index, is_inside_token) = self.find_cursor_token_index(&spans, cursor_pos);
        if !is_inside_token {
            return None;
        }

        let span = spans.get(index)?;
        Some((span.raw.clone(), cursor_pos.saturating_sub(span.byte_start)))
    }

    /// Split input string into tokens and keep their character spans.
    fn tokenize_with_positions(&self, input: &str) -> Vec<ShellTokenSpan> {
        shell_token::tokenize(input, SeparatorMode::Parser)
    }

    /// Find token index corresponding to cursor position (byte index).
    /// Returns (index, is_inside_token)
    /// If is_inside_token is true, we are appending/modifying tokens[index].
    /// If is_inside_token is false, we are inserting *before* tokens[index] (gap).
    fn find_cursor_token_index(
        &self,
        spans: &[ShellTokenSpan],
        cursor_pos: usize,
    ) -> (usize, bool) {
        for (i, span) in spans.iter().enumerate() {
            if cursor_pos <= span.byte_end {
                if cursor_pos >= span.byte_start {
                    // Inside or at the end of this token
                    return (i, true);
                } else {
                    // In the gap before this token
                    return (i, false);
                }
            }
        }
        // After all tokens
        (spans.len(), false)
    }

    /// Analyze tokens to determine completion context
    fn analyze_tokens(
        &self,
        tokens: Vec<String>,
        cursor_token_index: usize,
        input: &str,
        cursor_pos: usize,
    ) -> ParsedCommandLine {
        if tokens.is_empty() {
            return ParsedCommandLine {
                command: String::new(),
                subcommand_path: Vec::new(),
                raw_args: Vec::new(),
                args: Vec::new(),
                options: Vec::new(),
                current_token: String::new(),
                current_arg: None,
                completion_context: CompletionContext::Command,
                specified_options: Vec::new(),
                specified_arguments: Vec::new(),
                cursor_index: cursor_pos,
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
        while let Some(token) = tokens_queue.front() {
            if is_option_token(token) {
                break; // End subcommand parsing when options start
            }

            // Determine if it's an argument or subcommand (simplified version)
            // Treat consecutive "subcommand-like" tokens as subcommands.
            // Invalid ones will be reclassified as arguments by CompletionGenerator.
            if self.looks_like_subcommand(token) {
                if let Some(next_token) = tokens_queue.pop_front() {
                    subcommand_path.push(next_token);
                } else {
                    break;
                }
            } else {
                break; // End subcommand parsing when arguments start
            }
        }

        // Capture raw arguments (everything after subcommands)
        let raw_args: Vec<String> = tokens_queue.iter().cloned().collect();

        // Parse options and arguments
        let mut skip_next_option_value = false;
        let mut skip_next_redirect_target = false;
        let mut end_of_options = false;
        for (i, token) in tokens_queue.iter().enumerate() {
            if skip_next_option_value {
                skip_next_option_value = false;
                continue;
            }

            if skip_next_redirect_target {
                skip_next_redirect_target = false;
                continue;
            }

            if end_of_options {
                specified_arguments.push(token.clone());
                continue;
            }

            if token == "--" {
                end_of_options = true;
                continue;
            }

            if Self::is_redirect_operator(token) {
                skip_next_redirect_target = true;
                continue;
            }

            if is_option_token(token) {
                let option_name = split_inline_long_option(token)
                    .map(|(name, _)| name)
                    .unwrap_or(token);
                specified_options.push(option_name.to_string());

                // Check if next token is option value
                if split_inline_long_option(token).is_none()
                    && let Some(next_token) = tokens_queue.get(i + 1)
                    && !is_option_token(next_token)
                    && self.option_takes_value(option_name)
                {
                    skip_next_option_value = true;
                }

                continue;
            }

            specified_arguments.push(token.clone());
        }

        // Determine current token and completion context
        let (current_token, completion_context) = if cursor_token_index == 0 {
            (command.clone(), CompletionContext::Command)
        } else {
            let all_tokens: Vec<String> = std::iter::once(command.clone())
                .chain(subcommand_path.iter().cloned())
                .chain(tokens_queue.iter().cloned())
                .collect();

            let raw_current_token = if cursor_token_index < all_tokens.len() {
                all_tokens[cursor_token_index].clone()
            } else {
                String::new()
            };
            let after_end_of_options = all_tokens
                .iter()
                .take(cursor_token_index)
                .any(|token| token == "--");

            let context = self.determine_completion_context(CompletionContextParams {
                cursor_token_index,
                current_token: &raw_current_token,
                subcommand_path: &subcommand_path,
                _specified_options: &specified_options,
                specified_arguments: &specified_arguments,
                all_tokens: &all_tokens,
                has_space_after_command,
                after_end_of_options,
            });

            let current_token = if matches!(context, CompletionContext::OptionValue { .. })
                && let Some((_, value)) = split_inline_long_option(&raw_current_token)
            {
                value.to_string()
            } else {
                raw_current_token
            };

            (current_token, context)
        };

        // Create args and options for the unified structure
        let mut args = Vec::new();
        args.extend(specified_arguments.clone());

        let mut options = Vec::new();
        options.extend(specified_options.clone());

        ParsedCommandLine {
            command: command.clone(),
            subcommand_path: subcommand_path.clone(),
            raw_args,
            args,
            options,
            current_token: current_token.clone(),
            current_arg: Some(current_token.clone()),
            completion_context,
            specified_options: specified_options.clone(),
            specified_arguments: specified_arguments.clone(),
            cursor_index: cursor_pos,
        }
    }

    /// Check if there's a space after the command
    fn has_space_after_command(&self, input: &str, command: &str) -> bool {
        let trimmed = input.trim_start_matches(|c: char| c.is_whitespace());
        if !trimmed.starts_with(command) {
            return false;
        }
        trimmed
            .chars()
            .nth(command.chars().count())
            .is_some_and(|c| c.is_whitespace())
    }

    /// Determine if token looks like a subcommand
    fn looks_like_subcommand(&self, token: &str) -> bool {
        // Simple determination: not an option and not a file path
        if is_option_token(token) {
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

        // Be liberal here: most non-option tokens can be potential subcommands.
        // Invalid ones will be reclassified as arguments by CompletionGenerator.
        if token.len() < 2 || token.len() > 32 {
            return false;
        }

        // Check if it looks like typical command patterns rather than content
        // Commands tend to be verbs or short action words
        // Content tends to be nouns or descriptive words
        let chars: Vec<char> = token.chars().collect();

        // Count vowels and consonants
        let mut vowel_count = 0;
        let mut consonant_count = 0;

        for c in &chars {
            if matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u') {
                vowel_count += 1;
            } else if c.is_alphabetic() {
                consonant_count += 1;
            }
        }

        // Avoid words with alternating vowel-consonant patterns typical of content words
        // e.g. "file" (f-i-l-e) has alternating pattern: consonant-vowel-consonant-vowel
        // e.g. "data" (d-a-t-a) has alternating pattern: consonant-vowel-consonant-vowel
        if chars.len() == 4 && vowel_count == 2 && consonant_count == 2 {
            let mut vowel_positions = Vec::new();
            for c in &chars {
                vowel_positions.push(matches!(
                    c.to_ascii_lowercase(),
                    'a' | 'e' | 'i' | 'o' | 'u'
                ));
            }

            let mut alternating = true;
            for i in 0..vowel_positions.len() - 1 {
                if vowel_positions[i] == vowel_positions[i + 1] {
                    alternating = false;
                    break;
                }
            }

            // If it has an alternating pattern, it's more likely a content word
            if alternating {
                return false;
            }
        }

        // Allow alphanumeric characters, hyphens, and underscores
        token
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    }

    /// Determine completion context
    fn determine_completion_context(&self, params: CompletionContextParams) -> CompletionContext {
        if params.cursor_token_index == 0 {
            return CompletionContext::Command;
        }

        if params.after_end_of_options {
            return CompletionContext::Argument {
                arg_index: Self::argument_index(&params),
                arg_type: None,
            };
        }

        if let Some((option_name, _)) = split_inline_long_option(params.current_token) {
            return CompletionContext::OptionValue {
                option_name: option_name.to_string(),
                value_type: None,
            };
        }

        // If current token is an option
        if is_long_option_token(params.current_token) {
            return CompletionContext::LongOption;
        } else if is_option_token(params.current_token) {
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
            if is_option_token(prev_token) && self.option_takes_value(prev_token) {
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

        // Redirect target should trigger argument-like completion (file paths)
        if params.cursor_token_index > 0
            && let Some(prev_token) = params.all_tokens.get(params.cursor_token_index - 1)
            && Self::is_redirect_operator(prev_token)
        {
            return CompletionContext::Argument {
                arg_index: params.specified_arguments.len(),
                arg_type: None,
            };
        }

        if Self::is_redirect_operator(params.current_token) {
            return CompletionContext::Argument {
                arg_index: params.specified_arguments.len(),
                arg_type: None,
            };
        }

        if self.looks_like_path_argument(params.current_token) {
            return CompletionContext::Argument {
                arg_index: Self::argument_index(&params),
                arg_type: None,
            };
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
            CompletionContext::Argument {
                arg_index: Self::argument_index(&params),
                arg_type: None, // In actual implementation, get from completion data
            }
        }
    }

    fn argument_index(params: &CompletionContextParams) -> usize {
        params.specified_arguments.len().saturating_sub(
            if params
                .specified_arguments
                .contains(&params.current_token.to_string())
            {
                1
            } else {
                0
            },
        )
    }

    fn looks_like_path_argument(&self, token: &str) -> bool {
        token.contains('/')
            || token.contains('\\')
            || (token.contains('.')
                && token
                    .rfind('.')
                    .is_some_and(|i| i > 0 && i < token.len() - 1))
    }

    /// Determine if option takes a value (simplified version)
    fn option_takes_value(&self, option: &str) -> bool {
        // Options that commonly take values
        matches!(
            option,
            "--message" | "-m" 
            | "--target" 
            | "--features" 
            | "--git" 
            | "--path" 
            | "--name"
            | "-u" | "--user"    // sudo, etc
            | "-g" | "--group"   // sudo
            | "-n" | "--namespace" // kubectl
            | "--context"        // kubectl
            | "-C" | "--chdir"   // git, others
            | "-c"               // bash -c, etc
            | "-o" | "--output"  // gcc, etc
            | "-f" | "--file" // docker -f, etc
        )
    }

    /// Determine if token represents a redirect operator
    fn is_redirect_operator(token: &str) -> bool {
        if matches!(token, ">" | ">>" | "<" | "&>" | "&>>") {
            return true;
        }

        if let Some(prefix) = token.strip_suffix(">>") {
            return prefix.is_empty() || prefix.chars().all(|c| c.is_ascii_digit());
        }

        if let Some(prefix) = token.strip_suffix('>') {
            return prefix.is_empty() || prefix.chars().all(|c| c.is_ascii_digit());
        }

        false
    }
}

impl Default for CommandLineParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
