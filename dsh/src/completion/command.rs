#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Overall structure for command completion information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandCompletion {
    /// Command name (e.g., "git", "cargo", "docker")
    pub command: String,
    /// Command description
    pub description: Option<String>,
    /// List of subcommands
    pub subcommands: Vec<SubCommand>,
    /// Global options (common to all subcommands)
    pub global_options: Vec<CommandOption>,
}

/// Subcommand definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubCommand {
    /// Subcommand name (e.g., "add", "commit", "push")
    pub name: String,
    /// Subcommand description
    pub description: Option<String>,
    /// Options specific to this subcommand
    pub options: Vec<CommandOption>,
    /// Arguments that this subcommand accepts
    pub arguments: Vec<Argument>,
    /// Nested subcommands (e.g., git remote add)
    pub subcommands: Vec<SubCommand>,
    /// Aliases (short forms)
    pub aliases: Vec<String>,
}

/// Option definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOption {
    /// Short option (e.g., "-v")
    pub short: Option<String>,
    /// Long option (e.g., "--verbose")
    pub long: Option<String>,
    /// Option description
    pub description: Option<String>,
    /// Whether it takes a value (flag or parameter)
    pub takes_value: bool,
    /// Value type (when takes_value is true)
    pub value_type: Option<ArgumentType>,
    /// Whether it's a required option
    pub required: bool,
    /// 複数回指定可能かどうか
    pub multiple: bool,
}

/// 引数の定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Argument {
    /// 引数名
    pub name: String,
    /// 引数の説明
    pub description: Option<String>,
    /// 引数の型
    pub arg_type: ArgumentType,
    /// 必須引数かどうか
    pub required: bool,
    /// 複数の値を受け取るかどうか
    pub multiple: bool,
}

/// Argument/value types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum ArgumentType {
    /// File path
    File {
        /// File extension filter (e.g., [".rs", ".toml"])
        extensions: Option<Vec<String>>,
    },
    /// Directory path
    Directory,
    /// Arbitrary string
    String,
    /// Number
    Number,
    /// Choice from options
    Choice(Vec<String>),
    /// Existing command name
    Command,
    /// Environment variable name
    Environment,
    /// URL
    Url,
    /// Regular expression pattern
    Regex,
}

/// Command completion database
#[derive(Debug, Clone)]
pub struct CommandCompletionDatabase {
    /// Map of command name -> completion information
    commands: HashMap<String, CommandCompletion>,
}

impl CommandCompletionDatabase {
    /// Create a new empty database
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
        }
    }

    /// コマンド補完情報を追加
    pub fn add_command(&mut self, completion: CommandCompletion) {
        self.commands.insert(completion.command.clone(), completion);
    }

    /// コマンド名から補完情報を取得
    pub fn get_command(&self, command: &str) -> Option<&CommandCompletion> {
        self.commands.get(command)
    }

    /// 登録されているコマンド名の一覧を取得
    pub fn get_command_names(&self) -> Vec<&String> {
        self.commands.keys().collect()
    }

    /// データベースが空かどうか
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// 登録されているコマンド数
    pub fn len(&self) -> usize {
        self.commands.len()
    }
}

impl Default for CommandCompletionDatabase {
    fn default() -> Self {
        Self::new()
    }
}

/// 補完候補の種類
#[derive(Debug, Clone, PartialEq)]
pub enum CompletionType {
    /// サブコマンド
    SubCommand,
    /// オプション（短い形式）
    ShortOption,
    /// オプション（長い形式）
    LongOption,
    /// 引数値
    Argument,
    /// ファイル
    File,
    /// ディレクトリ
    Directory,
}

/// 補完候補
#[derive(Debug, Clone)]
pub struct CompletionCandidate {
    /// 候補文字列
    pub text: String,
    /// 説明
    pub description: Option<String>,
    /// 補完の種類
    pub completion_type: CompletionType,
    /// 優先度（高いほど上位に表示）
    pub priority: u32,
}

impl CompletionCandidate {
    /// 新しい補完候補を作成
    pub fn new(
        text: String,
        description: Option<String>,
        completion_type: CompletionType,
        priority: u32,
    ) -> Self {
        Self {
            text,
            description,
            completion_type,
            priority,
        }
    }

    /// サブコマンド候補を作成
    pub fn subcommand(name: String, description: Option<String>) -> Self {
        Self::new(name, description, CompletionType::SubCommand, 100)
    }

    /// 短いオプション候補を作成
    pub fn short_option(option: String, description: Option<String>) -> Self {
        Self::new(option, description, CompletionType::ShortOption, 80)
    }

    /// 長いオプション候補を作成
    pub fn long_option(option: String, description: Option<String>) -> Self {
        Self::new(option, description, CompletionType::LongOption, 80)
    }

    /// 引数候補を作成
    pub fn argument(value: String, description: Option<String>) -> Self {
        Self::new(value, description, CompletionType::Argument, 60)
    }

    /// ファイル候補を作成
    pub fn file(path: String) -> Self {
        Self::new(path, None, CompletionType::File, 40)
    }

    /// ディレクトリ候補を作成
    pub fn directory(path: String) -> Self {
        Self::new(path, None, CompletionType::Directory, 50)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_completion_database() {
        let mut db = CommandCompletionDatabase::new();
        assert!(db.is_empty());
        assert_eq!(db.len(), 0);

        let git_completion = CommandCompletion {
            command: "git".to_string(),
            description: Some("Git version control system".to_string()),
            subcommands: vec![],
            global_options: vec![],
        };

        db.add_command(git_completion);
        assert!(!db.is_empty());
        assert_eq!(db.len(), 1);
        assert!(db.get_command("git").is_some());
        assert!(db.get_command("nonexistent").is_none());
    }

    #[test]
    fn test_completion_candidate_creation() {
        let subcommand = CompletionCandidate::subcommand(
            "add".to_string(),
            Some("Add files to index".to_string()),
        );
        assert_eq!(subcommand.completion_type, CompletionType::SubCommand);
        assert_eq!(subcommand.priority, 100);

        let option = CompletionCandidate::long_option(
            "--verbose".to_string(),
            Some("Verbose output".to_string()),
        );
        assert_eq!(option.completion_type, CompletionType::LongOption);
        assert_eq!(option.priority, 80);
    }

    #[test]
    fn test_argument_type_serialization() {
        let file_type = ArgumentType::File {
            extensions: Some(vec![".rs".to_string(), ".toml".to_string()]),
        };
        let json = serde_json::to_string(&file_type).unwrap();
        let deserialized: ArgumentType = serde_json::from_str(&json).unwrap();

        match deserialized {
            ArgumentType::File { extensions } => {
                assert_eq!(
                    extensions,
                    Some(vec![".rs".to_string(), ".toml".to_string()])
                );
            }
            _ => panic!("Deserialization failed"),
        }
    }
}
