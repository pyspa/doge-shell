#![allow(dead_code)]
use super::command::CompletionCandidate;
use super::context::ContextCompletion;
use super::fuzzy::{FuzzyCompletion, SmartCompletion};
use super::generator::CompletionGenerator;
use super::history::{CompletionContext, HistoryCompletion};
use super::json_loader::JsonCompletionLoader;
use super::parser::CommandLineParser;
use crate::completion::Candidate;
use anyhow::Result;
use std::path::Path;
use tracing::{debug, info, warn};

/// 統合補完エンジン - 全ての補完機能を統合
pub struct IntegratedCompletionEngine {
    /// JSONベースのコマンド補完
    command_generator: Option<CompletionGenerator>,
    /// コマンドライン解析器
    parser: CommandLineParser,
    /// 既存の補完システム
    context_completion: ContextCompletion,
    fuzzy_completion: FuzzyCompletion,
    history_completion: HistoryCompletion,
    smart_completion: SmartCompletion,
}

impl IntegratedCompletionEngine {
    /// 新しい統合補完エンジンを作成
    pub fn new() -> Self {
        Self {
            command_generator: None,
            parser: CommandLineParser::new(),
            context_completion: ContextCompletion::new(),
            fuzzy_completion: FuzzyCompletion::new(),
            history_completion: HistoryCompletion::new(),
            smart_completion: SmartCompletion::new(),
        }
    }

    /// JSON補完データを初期化
    pub fn initialize_command_completion(&mut self) -> Result<()> {
        info!("Initializing command completion system...");

        debug!("Creating JsonCompletionLoader...");
        let loader = JsonCompletionLoader::new();
        
        debug!("Loading completion database...");
        match loader.load_database() {
            Ok(database) => {
                let command_count = database.len();
                info!("Loaded completion database with {} commands", command_count);
                
                if command_count > 0 {
                    debug!("Creating CompletionGenerator with database...");
                    self.command_generator = Some(CompletionGenerator::new(database));
                    info!(
                        "Command completion initialized successfully with {} commands",
                        command_count
                    );
                    
                    // デバッグ: 読み込まれたコマンドをリスト表示
                    if let Some(ref generator) = self.command_generator {
                        debug!("Available commands in database: {:?}", generator.get_available_commands());
                    }
                } else {
                    warn!("No command completion data found - completion database is empty");
                }
            }
            Err(e) => {
                warn!("Failed to load command completion data: {}", e);
                return Err(e);
            }
        }

        Ok(())
    }

    /// 履歴データを読み込み
    pub fn load_history(&mut self, history_path: &Path) -> Result<()> {
        self.history_completion.load_history(history_path)
    }

    /// 統合補完を実行
    pub fn complete(
        &self,
        input: &str,
        cursor_pos: usize,
        current_dir: &Path,
        max_results: usize,
    ) -> Vec<EnhancedCandidate> {
        debug!(
            "Integrated completion for: '{}' at position {} in {:?}",
            input, cursor_pos, current_dir
        );

        let mut all_candidates = Vec::new();

        // 1. 新しいコマンド補完システム（最高優先度）
        if let Some(ref generator) = self.command_generator {
            debug!("Using JSON completion generator for input: '{}'", input);
            let parsed = self.parser.parse(input, cursor_pos);
            debug!("Parsed command: {:?}", parsed);

            match generator.generate_candidates(&parsed) {
                Ok(command_candidates) => {
                    let enhanced_candidates = command_candidates
                        .into_iter()
                        .map(|c| self.convert_to_enhanced_candidate(c, CandidateSource::Command))
                        .collect::<Vec<_>>();

                    info!("JSON completion generated {} candidates for '{}'", enhanced_candidates.len(), input);
                    all_candidates.extend(enhanced_candidates);
                }
                Err(e) => {
                    warn!("Failed to generate JSON completion candidates: {}", e);
                }
            }
        } else {
            debug!("No JSON completion generator available - skipping JSON completion");
        }

        // 2. 既存のコンテキスト補完
        let parts: Vec<&str> = input.split_whitespace().collect();
        if !parts.is_empty() {
            let command = parts[0];
            let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

            let context_candidates =
                self.context_completion
                    .complete(command, &args, cursor_pos, current_dir);
            let enhanced_candidates = context_candidates
                .into_iter()
                .map(|c| self.convert_legacy_candidate(c, CandidateSource::Context))
                .collect::<Vec<_>>();

            debug!("Generated {} context candidates", enhanced_candidates.len());
            all_candidates.extend(enhanced_candidates);
        }

        // 3. 履歴ベース補完
        let context = CompletionContext::new(current_dir.to_string_lossy().to_string());
        let history_candidates = self.history_completion.complete_command(input, &context);
        let enhanced_candidates = history_candidates
            .into_iter()
            .map(|c| self.convert_legacy_candidate(c, CandidateSource::History))
            .collect::<Vec<_>>();

        debug!("Generated {} history candidates", enhanced_candidates.len());
        all_candidates.extend(enhanced_candidates);

        // 4. 重複除去とソート
        self.deduplicate_and_sort(all_candidates, max_results)
    }

    /// CompletionCandidateをEnhancedCandidateに変換
    fn convert_to_enhanced_candidate(
        &self,
        candidate: CompletionCandidate,
        source: CandidateSource,
    ) -> EnhancedCandidate {
        EnhancedCandidate {
            text: candidate.text,
            description: candidate.description,
            candidate_type: match candidate.completion_type {
                super::command::CompletionType::SubCommand => CandidateType::SubCommand,
                super::command::CompletionType::ShortOption => CandidateType::ShortOption,
                super::command::CompletionType::LongOption => CandidateType::LongOption,
                super::command::CompletionType::Argument => CandidateType::Argument,
                super::command::CompletionType::File => CandidateType::File,
                super::command::CompletionType::Directory => CandidateType::Directory,
            },
            priority: candidate.priority,
            source,
        }
    }

    /// 既存のCandidateをEnhancedCandidateに変換
    fn convert_legacy_candidate(
        &self,
        candidate: Candidate,
        source: CandidateSource,
    ) -> EnhancedCandidate {
        let (text, description, candidate_type) = match candidate {
            Candidate::Item(text, desc) => (text, Some(desc), CandidateType::Generic),
            Candidate::File { path, is_dir } => (
                path,
                None,
                if is_dir {
                    CandidateType::Directory
                } else {
                    CandidateType::File
                },
            ),
            Candidate::Path(path) => (path, None, CandidateType::File),
            Candidate::Basic(text) => (text, None, CandidateType::Generic),
            Candidate::Command { name, description } => {
                (name, Some(description), CandidateType::SubCommand)
            }
            Candidate::Option { name, description } => {
                (name, Some(description), CandidateType::LongOption)
            }
            Candidate::GitBranch { name, .. } => (name, None, CandidateType::Generic),
            Candidate::NpmScript { name } => (name, None, CandidateType::Generic),
            Candidate::History { command, .. } => (command, None, CandidateType::Generic),
        };

        EnhancedCandidate {
            text,
            description,
            candidate_type,
            priority: match source {
                CandidateSource::Command => 100,
                CandidateSource::Context => 80,
                CandidateSource::History => 60,
            },
            source,
        }
    }

    /// 重複除去とソート
    fn deduplicate_and_sort(
        &self,
        mut candidates: Vec<EnhancedCandidate>,
        max_results: usize,
    ) -> Vec<EnhancedCandidate> {
        // テキストベースで重複除去（優先度の高いものを残す）
        candidates.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.text.cmp(&b.text))
        });

        let mut seen = std::collections::HashSet::new();
        candidates.retain(|candidate| {
            if seen.contains(&candidate.text) {
                false
            } else {
                seen.insert(candidate.text.clone());
                true
            }
        });

        // 最終ソート（優先度 -> 種類 -> アルファベット順）
        candidates.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| {
                    a.candidate_type
                        .sort_order()
                        .cmp(&b.candidate_type.sort_order())
                })
                .then_with(|| a.text.cmp(&b.text))
        });

        candidates.truncate(max_results);
        candidates
    }
}

impl Default for IntegratedCompletionEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// 拡張された補完候補
#[derive(Debug, Clone)]
pub struct EnhancedCandidate {
    /// 候補テキスト
    pub text: String,
    /// 説明
    pub description: Option<String>,
    /// 候補の種類
    pub candidate_type: CandidateType,
    /// 優先度
    pub priority: u32,
    /// 候補の生成元
    pub source: CandidateSource,
}

/// 候補の種類
#[derive(Debug, Clone, PartialEq)]
pub enum CandidateType {
    /// サブコマンド
    SubCommand,
    /// 短いオプション
    ShortOption,
    /// 長いオプション
    LongOption,
    /// 引数
    Argument,
    /// ファイル
    File,
    /// ディレクトリ
    Directory,
    /// 汎用
    Generic,
}

impl CandidateType {
    /// ソート順序を取得
    pub fn sort_order(&self) -> u8 {
        match self {
            CandidateType::SubCommand => 1,
            CandidateType::LongOption => 2,
            CandidateType::ShortOption => 3,
            CandidateType::Argument => 4,
            CandidateType::Directory => 5,
            CandidateType::File => 6,
            CandidateType::Generic => 7,
        }
    }

    /// 表示用のアイコンを取得
    pub fn icon(&self) -> &'static str {
        match self {
            CandidateType::SubCommand => "⚡",
            CandidateType::LongOption => "🔧",
            CandidateType::ShortOption => "🔧",
            CandidateType::Argument => "📝",
            CandidateType::File => "📄",
            CandidateType::Directory => "📁",
            CandidateType::Generic => "💡",
        }
    }

    /// 表示用の色を取得
    pub fn color(&self) -> crossterm::style::Color {
        use crossterm::style::Color;
        match self {
            CandidateType::SubCommand => Color::Green,
            CandidateType::LongOption | CandidateType::ShortOption => Color::Blue,
            CandidateType::Argument => Color::Yellow,
            CandidateType::File => Color::White,
            CandidateType::Directory => Color::Cyan,
            CandidateType::Generic => Color::Grey,
        }
    }
}

/// 候補の生成元
#[derive(Debug, Clone, PartialEq)]
pub enum CandidateSource {
    /// コマンド補完システム
    Command,
    /// コンテキスト補完
    Context,
    /// 履歴補完
    History,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_integrated_completion_engine_creation() {
        let engine = IntegratedCompletionEngine::new();
        assert!(engine.command_generator.is_none());
    }

    #[test]
    fn test_candidate_type_sorting() {
        let mut types = [
            CandidateType::File,
            CandidateType::SubCommand,
            CandidateType::LongOption,
            CandidateType::Directory,
        ];

        types.sort_by_key(|t| t.sort_order());

        assert_eq!(types[0], CandidateType::SubCommand);
        assert_eq!(types[1], CandidateType::LongOption);
        assert_eq!(types[2], CandidateType::Directory);
        assert_eq!(types[3], CandidateType::File);
    }

    #[test]
    fn test_enhanced_candidate_creation() {
        let candidate = EnhancedCandidate {
            text: "test".to_string(),
            description: Some("Test command".to_string()),
            candidate_type: CandidateType::SubCommand,
            priority: 100,
            source: CandidateSource::Command,
        };

        assert_eq!(candidate.text, "test");
        assert_eq!(candidate.candidate_type.icon(), "⚡");
        assert_eq!(candidate.candidate_type.sort_order(), 1);
    }

    #[test]
    fn test_deduplication() {
        let engine = IntegratedCompletionEngine::new();

        let candidates = vec![
            EnhancedCandidate {
                text: "test".to_string(),
                description: None,
                candidate_type: CandidateType::SubCommand,
                priority: 100,
                source: CandidateSource::Command,
            },
            EnhancedCandidate {
                text: "test".to_string(),
                description: Some("Different description".to_string()),
                candidate_type: CandidateType::Generic,
                priority: 50,
                source: CandidateSource::History,
            },
        ];

        let result = engine.deduplicate_and_sort(candidates, 10);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].priority, 100); // 高い優先度が残る
        assert_eq!(result[0].source, CandidateSource::Command);
    }
}
