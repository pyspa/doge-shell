use super::Repl;
use crate::completion::shell_token::{self, SeparatorMode};
use crate::completion::{self as completion_lib};
use crate::input::ColorType;
use crate::parser::{self, Rule, ShellParser};
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use pest::Parser;

/// Result of input analysis, containing syntax highlighting and completion info
#[derive(Clone)]
pub(crate) struct InputAnalysis {
    pub(crate) completion_full: Option<String>,
    pub(crate) completion: Option<String>,
    pub(crate) color_ranges: Option<Vec<(usize, usize, ColorType)>>,
    pub(crate) can_execute: bool,
}

/// Cached redraw only needs completion metadata; color ranges already live on
/// `Input` until the input text changes.
pub(crate) struct CachedInputAnalysis {
    pub(crate) completion_full: Option<String>,
    pub(crate) completion: Option<String>,
}

/// Analyze shell input for syntax highlighting and inline completion.
///
/// This function parses the input via the Pest grammar, determines completion
/// candidates (command name or argument/path), and computes color ranges for
/// syntax highlighting.
pub(crate) fn analyze_input(
    repl: &Repl<'_>,
    input: &str,
    mut completion: Option<String>,
) -> InputAnalysis {
    // Skip syntax highlighting for AI commands (starting with !)
    if input.starts_with('!') {
        return InputAnalysis {
            completion_full: None,
            completion: None,
            color_ranges: None,
            can_execute: true,
        };
    }

    match ShellParser::parse(Rule::commands, input) {
        Ok(pairs) => {
            // 1. Get words for completion check
            let words = repl.input.get_words_from_pairs(pairs.clone());
            let mut completion_full = None;

            for (ref rule, ref span, current) in words {
                let word = span.as_str();
                if word.is_empty() {
                    continue;
                }

                match rule {
                    Rule::argv0
                        // Completion logic for command names
                        if current && completion.is_none() => {
                            if let Some(file) = repl.shell.environment.read().search_prefix(word) {
                                if file.len() >= input.len() && file.starts_with(input) {
                                    completion = Some(file[input.len()..].to_string());
                                }
                                completion_full = Some(file);
                                break;
                            } else if let Some(path_completion) =
                                super::completion::complete_path_for_span(
                                    input,
                                    span.start(),
                                    span.end(),
                                    true,
                                )
                            {
                                completion = Some(path_completion.suffix);
                                completion_full = Some(path_completion.full);
                                break;
                            }
                        }
                    Rule::args => {
                        // Completion logic for arguments
                        if current
                            && completion.is_none()
                            && let Some(path_completion) =
                                super::completion::complete_path_for_span(
                                    input,
                                    span.start(),
                                    span.end(),
                                    false,
                                )
                        {
                            completion = Some(path_completion.suffix);
                            completion_full = Some(path_completion.full);
                            break;
                        }
                    }
                    _ => {
                        // For other rule types, leave them with default color
                    }
                }
            }

            // 2. Compute color ranges using the same pairs
            let (mut color_ranges, can_execute) =
                repl.compute_color_ranges_from_pairs(pairs, input);

            apply_cached_path_highlighting(input, &mut color_ranges);
            append_cached_tail_path_highlighting(input, &mut color_ranges);

            InputAnalysis {
                completion_full,
                completion,
                color_ranges: Some(color_ranges),
                can_execute,
            }
        }
        Err(err) => {
            // Parsing failed, highlight the error
            let mut ranges = Vec::new();
            if let Some(range) = cached_tail_argument_path_range(input) {
                ranges.push(range);
            } else if let Some(token) = parser::highlight_error_token(input, err.location) {
                ranges.push((token.start, token.end, ColorType::Error));
            }
            InputAnalysis {
                completion_full: None,
                completion: None,
                color_ranges: Some(ranges),
                can_execute: false,
            }
        }
    }
}

fn apply_cached_path_highlighting(input: &str, color_ranges: &mut [(usize, usize, ColorType)]) {
    for (start, end, kind) in color_ranges.iter_mut() {
        if is_path_like_argument_color(*kind)
            && completion_lib::is_path_cached_for_shell_token(&input[*start..*end])
        {
            *kind = ColorType::ValidPath;
        }
    }
}

fn cached_tail_argument_path_range(input: &str) -> Option<(usize, usize, ColorType)> {
    let cursor = input.chars().count();
    let token = shell_token::token_at_char_cursor(input, cursor, SeparatorMode::CompletionRange)?;
    if input[..token.byte_start].trim().is_empty() {
        return None;
    }
    if completion_lib::is_path_cached_for_shell_token(&token.raw) {
        Some((token.byte_start, token.byte_end, ColorType::ValidPath))
    } else {
        None
    }
}

fn append_cached_tail_path_highlighting(
    input: &str,
    color_ranges: &mut Vec<(usize, usize, ColorType)>,
) {
    let Some((start, end, kind)) = cached_tail_argument_path_range(input) else {
        return;
    };
    if color_ranges
        .iter()
        .any(|(range_start, range_end, _)| ranges_overlap(start, end, *range_start, *range_end))
    {
        return;
    }

    color_ranges.push((start, end, kind));
    color_ranges.sort_by_key(|(start, _, _)| *start);
}

fn ranges_overlap(
    left_start: usize,
    left_end: usize,
    right_start: usize,
    right_end: usize,
) -> bool {
    left_start < right_end && right_start < left_end
}

fn is_path_like_argument_color(kind: ColorType) -> bool {
    matches!(
        kind,
        ColorType::Argument | ColorType::SingleQuote | ColorType::DoubleQuote
    )
}

use std::collections::HashMap;

/// Cache for command validity checks during syntax highlighting
pub(crate) struct CommandValidityCache {
    cache: HashMap<String, bool>,
}

impl CommandValidityCache {
    pub(crate) fn new() -> Self {
        Self {
            cache: HashMap::with_capacity(8),
        }
    }

    pub(crate) fn is_valid(&mut self, repl: &Repl<'_>, word: &str) -> bool {
        if let Some(&result) = self.cache.get(word) {
            return result;
        }
        let result = command_is_valid(repl, word);
        self.cache.insert(word.to_string(), result);
        result
    }
}

/// Check whether the given word is a valid (known) command.
///
/// This looks up the word against:
/// 1. PATH executables (via environment lookup)
/// 2. Shell aliases
/// 3. Built-in commands
/// 4. Lisp exports
pub(crate) fn command_is_valid(repl: &Repl<'_>, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }

    {
        let env = repl.shell.environment.read();
        if env.lookup(word).is_some() {
            return true;
        }

        if env.alias.contains_key(word) {
            return true;
        }
    }

    if dsh_builtin::get_command(word).is_some() {
        return true;
    }

    repl.shell.lisp_engine.borrow().is_export(word)
}

/// Toggle the `sudo` prefix on the current input line.
///
/// If the input starts with `sudo `, remove it. Otherwise, prepend `sudo `.
pub(crate) async fn toggle_sudo(repl: &mut Repl<'_>) -> Result<()> {
    let mut input = repl.input.as_str().to_string();
    if input.starts_with("sudo ") {
        input = input[5..].to_string();
    } else {
        input.insert_str(0, "sudo ");
    }
    repl.input.reset(input);
    let mut renderer = TerminalRenderer::new();
    repl.print_input(&mut renderer, true, true);
    renderer.flush().ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::shell::Shell;

    fn new_repl(shell: &mut Shell) -> Repl<'_> {
        Repl::new(shell)
    }

    fn cache_spaced_file() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("dir with space");
        std::fs::create_dir(&spaced).unwrap();
        std::fs::File::create(spaced.join("foo.txt")).unwrap();
        completion_lib::path_completion_path_sync(spaced.clone()).unwrap();
        (dir, spaced)
    }

    fn range_with_text<'a>(
        analysis: &'a InputAnalysis,
        input: &'a str,
        raw: &str,
    ) -> Option<(usize, usize, ColorType)> {
        analysis
            .color_ranges
            .as_ref()?
            .iter()
            .copied()
            .find(|(start, end, _)| &input[*start..*end] == raw)
    }

    #[tokio::test]
    async fn argument_path_suggestion_preserves_escaped_token_style() {
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("dir with space");
        std::fs::create_dir(&spaced).unwrap();
        std::fs::File::create(spaced.join("foo.txt")).unwrap();

        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = new_repl(&mut shell);

        let input = format!("ls {}/dir\\ with\\ space/fo", dir.path().display());
        let expected = format!("ls {}/dir\\ with\\ space/foo.txt", dir.path().display());
        repl.input.reset(input.clone());

        let analysis = analyze_input(&repl, &input, None);

        assert_eq!(analysis.completion_full.as_deref(), Some(expected.as_str()));
        assert_eq!(analysis.completion.as_deref(), Some("o.txt"));
    }

    #[tokio::test]
    async fn command_path_suggestion_preserves_escaped_token_style() {
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("dir with space");
        std::fs::create_dir(&spaced).unwrap();
        std::fs::create_dir(spaced.join("foodir")).unwrap();

        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = new_repl(&mut shell);

        let input = format!("{}/dir\\ with\\ space/fo", dir.path().display());
        let expected = format!("{}/dir\\ with\\ space/foodir/", dir.path().display());
        repl.input.reset(input.clone());

        let analysis = analyze_input(&repl, &input, None);

        assert_eq!(analysis.completion_full.as_deref(), Some(expected.as_str()));
        assert_eq!(analysis.completion.as_deref(), Some("odir/"));
    }

    #[tokio::test]
    async fn valid_path_highlighting_decodes_escaped_argument_token() {
        let (dir, _) = cache_spaced_file();
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = new_repl(&mut shell);

        let raw = format!("{}/dir\\ with\\ space/foo.txt", dir.path().display());
        let input = format!("ls {raw}");
        repl.input.reset(input.clone());

        let analysis = analyze_input(&repl, &input, None);
        let range = range_with_text(&analysis, &input, &raw).unwrap();

        assert!(matches!(range.2, ColorType::ValidPath));
    }

    #[tokio::test]
    async fn valid_path_highlighting_decodes_quoted_argument_token() {
        let (dir, _) = cache_spaced_file();
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = new_repl(&mut shell);

        let raw = format!("\"{}/dir with space/foo.txt\"", dir.path().display());
        let input = format!("ls {raw}");
        repl.input.reset(input.clone());

        let analysis = analyze_input(&repl, &input, None);
        let range = range_with_text(&analysis, &input, &raw).unwrap();

        assert!(matches!(range.2, ColorType::ValidPath));
    }

    #[tokio::test]
    async fn valid_path_highlighting_decodes_unclosed_quoted_argument_token() {
        let (dir, _) = cache_spaced_file();
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = new_repl(&mut shell);

        let raw = format!("\"{}/dir with space/foo.txt", dir.path().display());
        let input = format!("ls {raw}");
        repl.input.reset(input.clone());

        assert!(completion_lib::is_path_cached_for_shell_token(&raw));
        let analysis = analyze_input(&repl, &input, None);
        let range = range_with_text(&analysis, &input, &raw).unwrap();

        assert!(matches!(range.2, ColorType::ValidPath));
    }

    #[tokio::test]
    async fn valid_path_highlighting_leaves_non_path_argument_unchanged() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = new_repl(&mut shell);

        let raw = "not-a-cached-path";
        let input = format!("ls {raw}");
        repl.input.reset(input.clone());

        let analysis = analyze_input(&repl, &input, None);
        let range = range_with_text(&analysis, &input, raw).unwrap();

        assert!(matches!(range.2, ColorType::Argument));
    }
}
