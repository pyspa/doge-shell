#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// コマンド補完情報の全体構造
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandCompletion {
    /// コマンド名（例: "git", "cargo", "docker"）
    pub command: String,
    /// コマンドの説明
    pub description: Option<String>,
    /// サブコマンドのリスト
    pub subcommands: Vec<SubCommand>,
    /// グローバルオプション（全サブコマンドで共通）
    pub global_options: Vec<CommandOption>,
}

/// サブコマンドの定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubCommand {
    /// サブコマンド名（例: "add", "commit", "push"）
    pub name: String,
    /// サブコマンドの説明
    pub description: Option<String>,
    /// このサブコマンド固有のオプション
    pub options: Vec<CommandOption>,
    /// このサブコマンドが受け取る引数
    pub arguments: Vec<Argument>,
    /// ネストしたサブコマンド（例: git remote add）
    pub subcommands: Vec<SubCommand>,
    /// エイリアス（短縮形）
    pub aliases: Vec<String>,
}

/// オプションの定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOption {
    /// 短いオプション（例: "-v"）
    pub short: Option<String>,
    /// 長いオプション（例: "--verbose"）
    pub long: Option<String>,
    /// オプションの説明
    pub description: Option<String>,
    /// 値を取るかどうか（フラグかパラメータか）
    pub takes_value: bool,
    /// 値の型（takes_valueがtrueの場合）
    pub value_type: Option<ArgumentType>,
    /// 必須オプションかどうか
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

/// 引数・値の型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum ArgumentType {
    /// ファイルパス
    File {
        /// ファイル拡張子のフィルタ（例: [".rs", ".toml"]）
        extensions: Option<Vec<String>>,
    },
    /// ディレクトリパス
    Directory,
    /// 任意の文字列
    String,
    /// 数値
    Number,
    /// 選択肢から選ぶ
    Choice(Vec<String>),
    /// 既存のコマンド名
    Command,
    /// 環境変数名
    Environment,
    /// URL
    Url,
    /// 正規表現パターン
    Regex,
}

/// コマンド補完データベース
#[derive(Debug, Clone)]
pub struct CommandCompletionDatabase {
    /// コマンド名 -> 補完情報のマップ
    commands: HashMap<String, CommandCompletion>,
}

impl CommandCompletionDatabase {
    /// 新しい空のデータベースを作成
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
