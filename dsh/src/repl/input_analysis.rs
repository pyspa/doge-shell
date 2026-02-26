use super::Repl;
use crate::completion::{self as completion_lib};
use crate::dirs;
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
                    Rule::argv0 => {
                        // Completion logic for command names
                        if current && completion.is_none() {
                            if let Some(file) = repl.shell.environment.read().search(word) {
                                if file.len() >= input.len() && file.starts_with(input) {
                                    completion = Some(file[input.len()..].to_string());
                                }
                                completion_full = Some(file);
                                break;
                            } else if let Ok(Some(dir)) =
                                completion_lib::path_completion_prefix(word)
                                && dirs::is_dir(&dir)
                            {
                                if dir.len() >= input.len() && dir.starts_with(input) {
                                    completion = Some(dir[input.len()..].to_string());
                                }
                                completion_full = Some(dir.to_string());
                                break;
                            }
                        }
                    }
                    Rule::args => {
                        // Completion logic for arguments
                        if current
                            && completion.is_none()
                            && let Ok(Some(path)) = completion_lib::path_completion_prefix(word)
                            && path.len() >= word.len()
                            && path.starts_with(word)
                        {
                            let part = path[word.len()..].to_string();
                            completion = Some(part.clone());

                            if let Some((pre, post)) = repl.input.split_current_pos() {
                                completion_full = Some(pre.to_owned() + &part + post);
                            } else {
                                completion_full = Some(input.to_string() + &part);
                            }
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

            // Apply visual improvements for valid paths
            for (start, end, kind) in color_ranges.iter_mut() {
                // Check if Argument is a valid path
                if matches!(kind, ColorType::Argument) {
                    let word = &input[*start..*end];
                    // Clean up quotes if present for path check
                    let clean_word = word.trim_matches(|c| c == '\'' || c == '"');
                    let path = std::path::Path::new(clean_word);
                    if completion_lib::is_path_cached(path) {
                        *kind = ColorType::ValidPath;
                    }
                }
            }

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
            if let Some(token) = parser::highlight_error_token(input, err.location) {
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
