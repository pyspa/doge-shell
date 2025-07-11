#![allow(dead_code)]
use super::command::{
    ArgumentType, CommandCompletion, CommandCompletionDatabase, CommandOption, CompletionCandidate,
    SubCommand,
};
use super::parser::{CompletionContext, ParsedCommand};
use anyhow::Result;
use std::fs;
use std::path::Path;

/// 補完候補生成器
pub struct CompletionGenerator {
    /// コマンド補完データベース
    database: CommandCompletionDatabase,
}

impl CompletionGenerator {
    /// 新しい生成器を作成
    pub fn new(database: CommandCompletionDatabase) -> Self {
        Self { database }
    }

    /// 利用可能なコマンド一覧を取得（デバッグ用）
    pub fn get_available_commands(&self) -> Vec<String> {
        self.database
            .get_command_names()
            .into_iter()
            .cloned()
            .collect()
    }

    /// 解析されたコマンドから補完候補を生成
    pub fn generate_candidates(&self, parsed: &ParsedCommand) -> Result<Vec<CompletionCandidate>> {
        match &parsed.completion_context {
            CompletionContext::Command => self.generate_command_candidates(&parsed.current_token),
            CompletionContext::SubCommand => self.generate_subcommand_candidates(parsed),
            CompletionContext::ShortOption => self.generate_short_option_candidates(parsed),
            CompletionContext::LongOption => self.generate_long_option_candidates(parsed),
            CompletionContext::OptionValue {
                option_name,
                value_type,
            } => self.generate_option_value_candidates(parsed, option_name, value_type.as_ref()),
            CompletionContext::Argument {
                arg_index,
                arg_type,
            } => self.generate_argument_candidates(parsed, *arg_index, arg_type.as_ref()),
            CompletionContext::Unknown => Ok(Vec::new()),
        }
    }

    /// コマンド名の補完候補を生成
    fn generate_command_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        // データベースに登録されているコマンド
        for command_name in self.database.get_command_names() {
            if command_name.starts_with(current_token) {
                if let Some(completion) = self.database.get_command(command_name) {
                    candidates.push(CompletionCandidate::subcommand(
                        command_name.clone(),
                        completion.description.clone(),
                    ));
                }
            }
        }

        // システムのコマンドも追加（簡易版）
        candidates.extend(self.generate_system_command_candidates(current_token)?);

        Ok(candidates)
    }

    /// サブコマンドの補完候補を生成
    fn generate_subcommand_candidates(
        &self,
        parsed: &ParsedCommand,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            let current_subcommand =
                self.find_current_subcommand(command_completion, &parsed.subcommand_path);

            if let Some(subcommand) = current_subcommand {
                // ネストしたサブコマンドの候補
                for sub in &subcommand.subcommands {
                    if sub.name.starts_with(&parsed.current_token) {
                        candidates.push(CompletionCandidate::subcommand(
                            sub.name.clone(),
                            sub.description.clone(),
                        ));
                    }

                    // エイリアスも含める
                    for alias in &sub.aliases {
                        if alias.starts_with(&parsed.current_token) {
                            candidates.push(CompletionCandidate::subcommand(
                                alias.clone(),
                                Some(format!("Alias for {}", sub.name)),
                            ));
                        }
                    }
                }
            } else {
                // トップレベルのサブコマンド
                for subcommand in &command_completion.subcommands {
                    if subcommand.name.starts_with(&parsed.current_token) {
                        candidates.push(CompletionCandidate::subcommand(
                            subcommand.name.clone(),
                            subcommand.description.clone(),
                        ));
                    }

                    // エイリアスも含める
                    for alias in &subcommand.aliases {
                        if alias.starts_with(&parsed.current_token) {
                            candidates.push(CompletionCandidate::subcommand(
                                alias.clone(),
                                Some(format!("Alias for {}", subcommand.name)),
                            ));
                        }
                    }
                }
            }
        }

        Ok(candidates)
    }

    /// 短いオプションの補完候補を生成
    fn generate_short_option_candidates(
        &self,
        parsed: &ParsedCommand,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            let options =
                self.collect_available_options(command_completion, &parsed.subcommand_path);

            for option in options {
                if let Some(ref short) = option.short {
                    if short.starts_with(&parsed.current_token)
                        && !parsed.specified_options.contains(short)
                    {
                        candidates.push(CompletionCandidate::short_option(
                            short.clone(),
                            option.description.clone(),
                        ));
                    }
                }
            }
        }

        Ok(candidates)
    }

    /// 長いオプションの補完候補を生成
    fn generate_long_option_candidates(
        &self,
        parsed: &ParsedCommand,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        if let Some(command_completion) = self.database.get_command(&parsed.command) {
            let options =
                self.collect_available_options(command_completion, &parsed.subcommand_path);

            for option in options {
                if let Some(ref long) = option.long {
                    if long.starts_with(&parsed.current_token)
                        && !parsed.specified_options.contains(long)
                    {
                        candidates.push(CompletionCandidate::long_option(
                            long.clone(),
                            option.description.clone(),
                        ));
                    }
                }
            }
        }

        Ok(candidates)
    }

    /// オプション値の補完候補を生成
    fn generate_option_value_candidates(
        &self,
        parsed: &ParsedCommand,
        option_name: &str,
        value_type: Option<&ArgumentType>,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        // 実際の値の型を取得
        let actual_value_type = if let Some(vt) = value_type {
            Some(vt)
        } else {
            self.get_option_value_type(&parsed.command, &parsed.subcommand_path, option_name)
        };

        if let Some(arg_type) = actual_value_type {
            candidates.extend(self.generate_candidates_for_type(arg_type, &parsed.current_token)?);
        }

        Ok(candidates)
    }

    /// 引数の補完候補を生成
    fn generate_argument_candidates(
        &self,
        parsed: &ParsedCommand,
        arg_index: usize,
        arg_type: Option<&ArgumentType>,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        // 実際の引数の型を取得
        let actual_arg_type = if let Some(at) = arg_type {
            Some(at)
        } else {
            self.get_argument_type(&parsed.command, &parsed.subcommand_path, arg_index)
        };

        if let Some(arg_type) = actual_arg_type {
            candidates.extend(self.generate_candidates_for_type(arg_type, &parsed.current_token)?);
        } else {
            // デフォルトでファイル補完
            candidates.extend(self.generate_file_candidates(&parsed.current_token)?);
        }

        Ok(candidates)
    }

    /// 型に基づいて補完候補を生成
    fn generate_candidates_for_type(
        &self,
        arg_type: &ArgumentType,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        match arg_type {
            ArgumentType::File { extensions } => {
                self.generate_file_candidates_with_filter(current_token, extensions.as_ref())
            }
            ArgumentType::Directory => self.generate_directory_candidates(current_token),
            ArgumentType::Choice(choices) => Ok(choices
                .iter()
                .filter(|choice| choice.starts_with(current_token))
                .map(|choice| CompletionCandidate::argument(choice.clone(), None))
                .collect()),
            ArgumentType::Command => self.generate_system_command_candidates(current_token),
            ArgumentType::Environment => {
                self.generate_environment_variable_candidates(current_token)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// ファイル補完候補を生成
    fn generate_file_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        self.generate_file_candidates_with_filter(current_token, None)
    }

    /// フィルタ付きファイル補完候補を生成
    fn generate_file_candidates_with_filter(
        &self,
        current_token: &str,
        extensions: Option<&Vec<String>>,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        let (dir_path, file_prefix) = if current_token.contains('/') {
            let path = Path::new(current_token);
            if let Some(parent) = path.parent() {
                (
                    parent.to_string_lossy().to_string(),
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string(),
                )
            } else {
                (".".to_string(), current_token.to_string())
            }
        } else {
            (".".to_string(), current_token.to_string())
        };

        if let Ok(entries) = fs::read_dir(&dir_path) {
            for entry in entries.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();

                if file_name.starts_with(&file_prefix) {
                    let path = entry.path();

                    // 拡張子フィルタ
                    if let Some(exts) = extensions {
                        if path.is_file() {
                            if let Some(ext) = path.extension() {
                                let ext_str = format!(".{}", ext.to_string_lossy());
                                if !exts.contains(&ext_str) {
                                    continue;
                                }
                            } else {
                                continue;
                            }
                        }
                    }

                    let full_path = if dir_path == "." {
                        file_name
                    } else {
                        format!("{}/{}", dir_path, file_name)
                    };

                    if path.is_dir() {
                        candidates.push(CompletionCandidate::directory(full_path));
                    } else {
                        candidates.push(CompletionCandidate::file(full_path));
                    }
                }
            }
        }

        Ok(candidates)
    }

    /// ディレクトリ補完候補を生成
    fn generate_directory_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        let (dir_path, dir_prefix) = if current_token.contains('/') {
            let path = Path::new(current_token);
            if let Some(parent) = path.parent() {
                (
                    parent.to_string_lossy().to_string(),
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string(),
                )
            } else {
                (".".to_string(), current_token.to_string())
            }
        } else {
            (".".to_string(), current_token.to_string())
        };

        if let Ok(entries) = fs::read_dir(&dir_path) {
            for entry in entries.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();

                if file_name.starts_with(&dir_prefix) && entry.path().is_dir() {
                    let full_path = if dir_path == "." {
                        file_name
                    } else {
                        format!("{}/{}", dir_path, file_name)
                    };

                    candidates.push(CompletionCandidate::directory(full_path));
                }
            }
        }

        Ok(candidates)
    }

    /// システムコマンドの補完候補を生成（簡易版）
    fn generate_system_command_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        // List of common commands (in actual implementation, get from PATH)
        let common_commands = [
            "ls", "cd", "pwd", "mkdir", "rmdir", "rm", "cp", "mv", "cat", "less", "more", "grep",
            "find", "which", "whereis", "man", "help", "echo", "printf", "git", "cargo", "rustc",
            "npm", "node", "python", "python3", "pip", "docker", "kubectl", "ssh", "scp", "curl",
            "wget", "tar", "zip", "unzip",
        ];

        for cmd in &common_commands {
            if cmd.starts_with(current_token) {
                candidates.push(CompletionCandidate::subcommand(cmd.to_string(), None));
            }
        }

        Ok(candidates)
    }

    /// 環境変数の補完候補を生成
    fn generate_environment_variable_candidates(
        &self,
        current_token: &str,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        for (key, _) in std::env::vars() {
            if key.starts_with(current_token) {
                candidates.push(CompletionCandidate::argument(key, None));
            }
        }

        Ok(candidates)
    }

    /// 現在のサブコマンドを見つける
    fn find_current_subcommand<'a>(
        &self,
        command_completion: &'a CommandCompletion,
        subcommand_path: &[String],
    ) -> Option<&'a SubCommand> {
        if subcommand_path.is_empty() {
            return None;
        }

        let mut current_subcommands = &command_completion.subcommands;
        let mut current_subcommand = None;

        for subcommand_name in subcommand_path {
            current_subcommand = current_subcommands
                .iter()
                .find(|sc| sc.name == *subcommand_name || sc.aliases.contains(subcommand_name));

            if let Some(sc) = current_subcommand {
                current_subcommands = &sc.subcommands;
            } else {
                return None;
            }
        }

        current_subcommand
    }

    /// 利用可能なオプションを収集
    fn collect_available_options<'a>(
        &self,
        command_completion: &'a CommandCompletion,
        subcommand_path: &[String],
    ) -> Vec<&'a CommandOption> {
        let mut options = Vec::new();

        // グローバルオプション
        options.extend(&command_completion.global_options);

        // サブコマンドのオプション
        if let Some(subcommand) = self.find_current_subcommand(command_completion, subcommand_path)
        {
            options.extend(&subcommand.options);
        }

        options
    }

    /// オプションの値の型を取得
    fn get_option_value_type(
        &self,
        command: &str,
        subcommand_path: &[String],
        option_name: &str,
    ) -> Option<&ArgumentType> {
        if let Some(command_completion) = self.database.get_command(command) {
            let options = self.collect_available_options(command_completion, subcommand_path);

            for option in options {
                if option.short.as_deref() == Some(option_name)
                    || option.long.as_deref() == Some(option_name)
                {
                    return option.value_type.as_ref();
                }
            }
        }
        None
    }

    /// 引数の型を取得
    fn get_argument_type(
        &self,
        command: &str,
        subcommand_path: &[String],
        arg_index: usize,
    ) -> Option<&ArgumentType> {
        if let Some(command_completion) = self.database.get_command(command) {
            if let Some(subcommand) =
                self.find_current_subcommand(command_completion, subcommand_path)
            {
                if let Some(arg) = subcommand.arguments.get(arg_index) {
                    return Some(&arg.arg_type);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::command::{Argument, CommandCompletion, SubCommand};

    fn create_test_database() -> CommandCompletionDatabase {
        let mut db = CommandCompletionDatabase::new();

        let git_completion = CommandCompletion {
            command: "git".to_string(),
            description: Some("Git version control".to_string()),
            global_options: vec![],
            subcommands: vec![
                SubCommand {
                    name: "add".to_string(),
                    description: Some("Add files".to_string()),
                    aliases: vec!["a".to_string()],
                    options: vec![],
                    arguments: vec![Argument {
                        name: "pathspec".to_string(),
                        description: Some("Files to add".to_string()),
                        arg_type: ArgumentType::File { extensions: None },
                        required: false,
                        multiple: true,
                    }],
                    subcommands: vec![],
                },
                SubCommand {
                    name: "commit".to_string(),
                    description: Some("Commit changes".to_string()),
                    aliases: vec![],
                    options: vec![],
                    arguments: vec![],
                    subcommands: vec![],
                },
            ],
        };

        db.add_command(git_completion);
        db
    }

    #[test]
    fn test_generate_command_candidates() {
        let db = create_test_database();
        let generator = CompletionGenerator::new(db);

        let candidates = generator.generate_command_candidates("gi").unwrap();
        assert!(!candidates.is_empty());

        let git_candidate = candidates.iter().find(|c| c.text == "git");
        assert!(git_candidate.is_some());
    }

    #[test]
    fn test_generate_subcommand_candidates() {
        let db = create_test_database();
        let generator = CompletionGenerator::new(db);

        let parsed = ParsedCommand {
            command: "git".to_string(),
            subcommand_path: vec![],
            current_token: "a".to_string(),
            completion_context: CompletionContext::SubCommand,
            specified_options: vec![],
            specified_arguments: vec![],
        };

        let candidates = generator.generate_subcommand_candidates(&parsed).unwrap();
        assert!(!candidates.is_empty());

        let add_candidate = candidates.iter().find(|c| c.text == "add");
        assert!(add_candidate.is_some());

        let alias_candidate = candidates.iter().find(|c| c.text == "a");
        assert!(alias_candidate.is_some());
    }
}
