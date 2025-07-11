#![allow(dead_code)]
use super::integrated::{CandidateType, EnhancedCandidate};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::{queue, terminal};
use std::io::{Result as IoResult, Write, stdout};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 補完候補の表示設定
#[derive(Debug, Clone)]
pub struct DisplayConfig {
    /// 最大表示行数
    pub max_rows: usize,
    /// 最大表示列数
    pub max_columns: usize,
    /// Whether to show descriptions
    pub show_descriptions: bool,
    /// Whether to show icons
    pub show_icons: bool,
    /// Whether to use color coding
    pub use_colors: bool,
    /// Maximum characters per line
    pub max_width_per_item: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            max_rows: 10,
            max_columns: 3,
            show_descriptions: true,
            show_icons: true,
            use_colors: true,
            max_width_per_item: 40,
        }
    }
}

/// 補完候補表示器
pub struct CompletionDisplay {
    config: DisplayConfig,
}

impl CompletionDisplay {
    /// 新しい表示器を作成
    pub fn new(config: DisplayConfig) -> Self {
        Self { config }
    }

    /// デフォルト設定で表示器を作成
    pub fn with_default_config() -> Self {
        Self::new(DisplayConfig::default())
    }

    /// 補完候補を表示
    pub fn display_candidates(&self, candidates: &[EnhancedCandidate]) -> IoResult<()> {
        if candidates.is_empty() {
            return Ok(());
        }

        let mut stdout = stdout();

        // ターミナルサイズを取得
        let (terminal_width, _) = terminal::size()?;
        let available_width = terminal_width as usize;

        // 候補を種類別にグループ化
        let grouped = self.group_candidates_by_type(candidates);

        // 各グループを表示
        for (candidate_type, group_candidates) in grouped {
            if group_candidates.is_empty() {
                continue;
            }

            // グループヘッダーを表示
            self.display_group_header(&mut stdout, &candidate_type)?;

            // 候補を表示
            self.display_candidate_group(&mut stdout, &group_candidates, available_width)?;

            // グループ間の空行
            queue!(stdout, Print("\n"))?;
        }

        stdout.flush()?;
        Ok(())
    }

    /// 候補を種類別にグループ化
    fn group_candidates_by_type<'a>(
        &self,
        candidates: &'a [EnhancedCandidate],
    ) -> Vec<(CandidateType, Vec<&'a EnhancedCandidate>)> {
        let mut groups: std::collections::BTreeMap<u8, (CandidateType, Vec<&EnhancedCandidate>)> =
            std::collections::BTreeMap::new();

        for candidate in candidates {
            let sort_order = candidate.candidate_type.sort_order();
            groups
                .entry(sort_order)
                .or_insert_with(|| (candidate.candidate_type.clone(), Vec::new()))
                .1
                .push(candidate);
        }

        groups.into_values().collect()
    }

    /// グループヘッダーを表示
    fn display_group_header(
        &self,
        stdout: &mut std::io::Stdout,
        candidate_type: &CandidateType,
    ) -> IoResult<()> {
        if !self.config.use_colors {
            queue!(
                stdout,
                Print(format!(
                    "{} {}:\n",
                    if self.config.show_icons {
                        candidate_type.icon()
                    } else {
                        ""
                    },
                    self.get_type_name(candidate_type)
                ))
            )?;
            return Ok(());
        }

        queue!(
            stdout,
            SetForegroundColor(candidate_type.color()),
            Print(format!(
                "{} {}:\n",
                if self.config.show_icons {
                    candidate_type.icon()
                } else {
                    ""
                },
                self.get_type_name(candidate_type)
            )),
            ResetColor
        )?;

        Ok(())
    }

    /// 候補グループを表示
    fn display_candidate_group(
        &self,
        stdout: &mut std::io::Stdout,
        candidates: &[&EnhancedCandidate],
        available_width: usize,
    ) -> IoResult<()> {
        let items_per_row = self.calculate_items_per_row(candidates, available_width);
        let max_items = self.config.max_rows * items_per_row;
        let display_candidates = &candidates[..candidates.len().min(max_items)];

        for chunk in display_candidates.chunks(items_per_row) {
            self.display_candidate_row(stdout, chunk, available_width)?;
        }

        // 省略された候補がある場合の表示
        if candidates.len() > max_items {
            queue!(
                stdout,
                SetForegroundColor(Color::DarkGrey),
                Print(format!("  ... and {} more\n", candidates.len() - max_items)),
                ResetColor
            )?;
        }

        Ok(())
    }

    /// Display one row of candidates
    fn display_candidate_row(
        &self,
        stdout: &mut std::io::Stdout,
        candidates: &[&EnhancedCandidate],
        available_width: usize,
    ) -> IoResult<()> {
        let column_width = available_width / candidates.len().max(1);

        for (i, candidate) in candidates.iter().enumerate() {
            if i > 0 {
                queue!(stdout, Print("  "))?; // 列間のスペース
            }

            self.display_single_candidate(stdout, candidate, column_width)?;
        }

        queue!(stdout, Print("\n"))?;
        Ok(())
    }

    /// 単一の候補を表示
    fn display_single_candidate(
        &self,
        stdout: &mut std::io::Stdout,
        candidate: &EnhancedCandidate,
        max_width: usize,
    ) -> IoResult<()> {
        let text = self.truncate_text(&candidate.text, max_width);

        if !self.config.use_colors {
            queue!(stdout, Print(format!("  {}", text)))?;
            return Ok(());
        }

        // 色付きで表示
        queue!(
            stdout,
            SetForegroundColor(candidate.candidate_type.color()),
            Print(format!("  {}", text)),
            ResetColor
        )?;

        // 説明文がある場合は薄い色で表示
        if self.config.show_descriptions {
            if let Some(ref description) = candidate.description {
                let desc_text =
                    self.truncate_text(description, max_width.saturating_sub(text.width() + 4));
                if !desc_text.is_empty() {
                    queue!(
                        stdout,
                        SetForegroundColor(Color::DarkGrey),
                        Print(format!(" ({})", desc_text)),
                        ResetColor
                    )?;
                }
            }
        }

        Ok(())
    }

    /// Calculate number of candidates per row
    fn calculate_items_per_row(
        &self,
        candidates: &[&EnhancedCandidate],
        available_width: usize,
    ) -> usize {
        if candidates.is_empty() {
            return 1;
        }

        // 最長の候補テキストの幅を計算
        let max_text_width = candidates
            .iter()
            .map(|c| c.text.width())
            .max()
            .unwrap_or(10);

        // 説明文も考慮
        let max_desc_width = if self.config.show_descriptions {
            candidates
                .iter()
                .filter_map(|c| c.description.as_ref())
                .map(|d| d.width())
                .max()
                .unwrap_or(0)
                .min(20) // Description text is max 20 characters
        } else {
            0
        };

        let estimated_item_width = max_text_width + max_desc_width + 6; // Margin and icon space
        let items_per_row = (available_width / estimated_item_width.max(1)).max(1);

        items_per_row.min(self.config.max_columns)
    }

    /// テキストを指定幅に切り詰め
    fn truncate_text(&self, text: &str, max_width: usize) -> String {
        if text.width() <= max_width {
            text.to_string()
        } else {
            let mut result = String::new();
            let mut current_width = 0;

            for ch in text.chars() {
                let ch_width = ch.width().unwrap_or(0);
                if current_width + ch_width + 3 > max_width {
                    // "..." 分を考慮
                    result.push_str("...");
                    break;
                }
                result.push(ch);
                current_width += ch_width;
            }

            result
        }
    }

    /// 候補種類の名前を取得
    fn get_type_name(&self, candidate_type: &CandidateType) -> &'static str {
        match candidate_type {
            CandidateType::SubCommand => "Subcommands",
            CandidateType::LongOption => "Options",
            CandidateType::ShortOption => "Short Options",
            CandidateType::Argument => "Arguments",
            CandidateType::File => "Files",
            CandidateType::Directory => "Directories",
            CandidateType::Generic => "Suggestions",
        }
    }

    /// インタラクティブな候補選択を表示
    pub fn display_interactive_selection(
        &self,
        candidates: &[EnhancedCandidate],
        selected_index: usize,
    ) -> IoResult<()> {
        let mut stdout = stdout();

        // 画面をクリア
        queue!(stdout, terminal::Clear(terminal::ClearType::FromCursorDown))?;

        for (i, candidate) in candidates.iter().enumerate() {
            let is_selected = i == selected_index;

            if is_selected {
                // 選択された候補をハイライト
                queue!(
                    stdout,
                    SetForegroundColor(Color::Black),
                    crossterm::style::SetBackgroundColor(Color::White),
                    Print(format!("▶ {}", candidate.text)),
                    ResetColor
                )?;
            } else {
                // 通常の候補
                queue!(
                    stdout,
                    SetForegroundColor(candidate.candidate_type.color()),
                    Print(format!("  {}", candidate.text)),
                    ResetColor
                )?;
            }

            // 説明文を表示
            if let Some(ref description) = candidate.description {
                queue!(
                    stdout,
                    SetForegroundColor(Color::DarkGrey),
                    Print(format!(" - {}", description)),
                    ResetColor
                )?;
            }

            queue!(stdout, Print("\n"))?;
        }

        stdout.flush()?;
        Ok(())
    }
}

/// 簡易表示関数（既存システムとの互換性のため）
pub fn display_candidates_simple(candidates: &[EnhancedCandidate]) -> IoResult<()> {
    let display = CompletionDisplay::with_default_config();
    display.display_candidates(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::integrated::CandidateSource;

    fn create_test_candidate(text: &str, candidate_type: CandidateType) -> EnhancedCandidate {
        EnhancedCandidate {
            text: text.to_string(),
            description: Some(format!("Description for {}", text)),
            candidate_type,
            priority: 100,
            source: CandidateSource::Command,
        }
    }

    #[test]
    fn test_display_config_default() {
        let config = DisplayConfig::default();
        assert_eq!(config.max_rows, 10);
        assert_eq!(config.max_columns, 3);
        assert!(config.show_descriptions);
        assert!(config.show_icons);
        assert!(config.use_colors);
    }

    #[test]
    fn test_group_candidates_by_type() {
        let display = CompletionDisplay::with_default_config();
        let candidates = vec![
            create_test_candidate("file.txt", CandidateType::File),
            create_test_candidate("add", CandidateType::SubCommand),
            create_test_candidate("--verbose", CandidateType::LongOption),
            create_test_candidate("dir/", CandidateType::Directory),
        ];

        let grouped = display.group_candidates_by_type(&candidates);

        // サブコマンドが最初に来る
        assert_eq!(grouped[0].0, CandidateType::SubCommand);
        assert_eq!(grouped[0].1.len(), 1);

        // オプションが次に来る
        assert_eq!(grouped[1].0, CandidateType::LongOption);
        assert_eq!(grouped[1].1.len(), 1);
    }

    #[test]
    fn test_truncate_text() {
        let display = CompletionDisplay::with_default_config();

        let short_text = "short";
        assert_eq!(display.truncate_text(short_text, 10), "short");

        let long_text = "this_is_a_very_long_text";
        let truncated = display.truncate_text(long_text, 10);
        assert!(truncated.len() <= 10);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn test_calculate_items_per_row() {
        let display = CompletionDisplay::with_default_config();
        let candidate1 = create_test_candidate("short", CandidateType::SubCommand);
        let candidate2 = create_test_candidate("medium_length", CandidateType::SubCommand);
        let candidates = vec![&candidate1, &candidate2];

        let items_per_row = display.calculate_items_per_row(&candidates, 80);
        assert!(items_per_row >= 1);
        assert!(items_per_row <= display.config.max_columns);
    }
}
