#![allow(dead_code)]
use super::command::ArgumentType;
use std::collections::VecDeque;

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
    /// 既に指定されたオプション
    pub specified_options: Vec<String>,
    /// 既に指定された引数
    pub specified_arguments: Vec<String>,
}

/// 補完のコンテキスト（現在どの部分を補完しようとしているか）
#[derive(Debug, Clone, PartialEq)]
pub enum CompletionContext {
    /// コマンド名を補完
    Command,
    /// サブコマンドを補完
    SubCommand,
    /// オプションを補完（短い形式 -x）
    ShortOption,
    /// オプションを補完（長い形式 --xxx）
    LongOption,
    /// オプションの値を補完
    OptionValue {
        option_name: String,
        value_type: Option<ArgumentType>,
    },
    /// 引数を補完
    Argument {
        arg_index: usize,
        arg_type: Option<ArgumentType>,
    },
    /// 不明（エラー状態）
    Unknown,
}

/// コマンドライン解析器
pub struct CommandLineParser;

impl CommandLineParser {
    /// 新しいパーサーを作成
    pub fn new() -> Self {
        Self
    }

    /// コマンドラインを解析
    pub fn parse(&self, input: &str, cursor_pos: usize) -> ParsedCommand {
        let tokens = self.tokenize(input);
        let cursor_token_index = self.find_cursor_token_index(&tokens, input, cursor_pos);

        self.analyze_tokens(tokens, cursor_token_index)
    }

    /// 入力文字列をトークンに分割
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

    /// カーソル位置に対応するトークンのインデックスを見つける
    fn find_cursor_token_index(&self, tokens: &[String], input: &str, cursor_pos: usize) -> usize {
        let mut pos = 0;

        for (i, token) in tokens.iter().enumerate() {
            // トークンの開始位置を見つける
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

        tokens.len() // カーソルが最後にある場合
    }

    /// トークンを解析して補完コンテキストを決定
    fn analyze_tokens(&self, tokens: Vec<String>, cursor_token_index: usize) -> ParsedCommand {
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

        // サブコマンドを解析
        let mut subcommand_count = 0;
        while let Some(token) = tokens_queue.front() {
            if token.starts_with('-') {
                break; // オプションが始まったらサブコマンド解析終了
            }

            // 引数かサブコマンドかを判定（簡易版）
            // 最初の数個のトークンのみサブコマンドとして扱う
            if subcommand_count < 2 && self.looks_like_subcommand(token) {
                subcommand_path.push(tokens_queue.pop_front().unwrap());
                subcommand_count += 1;
            } else {
                break; // 引数が始まったらサブコマンド解析終了
            }
        }

        // オプションと引数を解析
        let mut skip_next = false;
        for (i, token) in tokens_queue.iter().enumerate() {
            if skip_next {
                skip_next = false;
                continue;
            }

            if token.starts_with('-') {
                specified_options.push(token.clone());

                // 次のトークンがオプションの値かチェック
                if let Some(next_token) = tokens_queue.get(i + 1) {
                    if !next_token.starts_with('-') && self.option_takes_value(token) {
                        skip_next = true;
                    }
                }
            } else {
                specified_arguments.push(token.clone());
            }
        }

        // 現在のトークンと補完コンテキストを決定
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

            let context = self.determine_completion_context(
                cursor_token_index,
                &current_token,
                &subcommand_path,
                &specified_options,
                &specified_arguments,
                &all_tokens,
            );

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

    /// トークンがサブコマンドらしいかどうかを判定
    fn looks_like_subcommand(&self, token: &str) -> bool {
        // 簡易的な判定：オプションでなく、ファイルパスでもない
        if token.starts_with('-') {
            return false;
        }

        // ファイル拡張子がある場合はファイルとみなす
        if token.contains('.')
            && token
                .rfind('.')
                .is_some_and(|i| i > 0 && i < token.len() - 1)
        {
            return false;
        }

        // パスセパレータがある場合はファイルパスとみなす
        if token.contains('/') || token.contains('\\') {
            return false;
        }

        // 一般的なサブコマンド名のパターン（厳密版）
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

        // 既知のサブコマンドのみ許可（より厳密）
        common_subcommands.contains(&token)
    }

    /// 補完コンテキストを決定
    fn determine_completion_context(
        &self,
        cursor_token_index: usize,
        current_token: &str,
        subcommand_path: &[String],
        _specified_options: &[String],
        specified_arguments: &[String],
        all_tokens: &[String],
    ) -> CompletionContext {
        if cursor_token_index == 0 {
            return CompletionContext::Command;
        }

        // 現在のトークンがオプションの場合
        if current_token.starts_with("--") {
            return CompletionContext::LongOption;
        } else if current_token.starts_with('-') && current_token.len() == 2 {
            return CompletionContext::ShortOption;
        }

        // 直前のトークンがオプションで値を取る場合
        if cursor_token_index > 0 {
            let prev_token = &all_tokens[cursor_token_index - 1];
            if prev_token.starts_with('-') && self.option_takes_value(prev_token) {
                return CompletionContext::OptionValue {
                    option_name: prev_token.clone(),
                    value_type: None, // 実際の実装では補完データから取得
                };
            }
        }

        // サブコマンドまたは引数
        if subcommand_path.is_empty() || self.looks_like_subcommand(current_token) {
            CompletionContext::SubCommand
        } else {
            // 現在のトークンが引数の場合、そのインデックスを計算
            // 現在のトークンは含めない（補完対象なので）
            let arg_index = specified_arguments.len().saturating_sub(
                if specified_arguments.contains(&current_token.to_string()) {
                    1
                } else {
                    0
                },
            );
            CompletionContext::Argument {
                arg_index,
                arg_type: None, // 実際の実装では補完データから取得
            }
        }
    }

    /// オプションが値を取るかどうかを判定（簡易版）
    fn option_takes_value(&self, option: &str) -> bool {
        // 一般的に値を取るオプション
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
    fn test_parse_subcommand() {
        let parser = CommandLineParser::new();
        let result = parser.parse("git add", 7);

        assert_eq!(result.command, "git");
        assert_eq!(result.subcommand_path, vec!["add"]);
        assert_eq!(result.completion_context, CompletionContext::SubCommand);
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
}
