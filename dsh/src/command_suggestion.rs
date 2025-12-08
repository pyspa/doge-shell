//! Command suggestion module for typo correction
//!
//! Provides functionality to suggest similar commands when a typo is detected.
//! Uses Levenshtein distance algorithm to find commands with similar spelling.

use std::collections::HashSet;
use std::fs::read_dir;

/// Calculate Levenshtein distance between two strings
/// This is the minimum number of single-character edits (insertions, deletions, or substitutions)
/// required to change one string into the other.
pub fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    // Use two rows instead of full matrix for space efficiency
    let mut prev_row: Vec<usize> = (0..=b_len).collect();
    let mut curr_row: Vec<usize> = vec![0; b_len + 1];

    for (i, a_char) in a_chars.iter().enumerate() {
        curr_row[0] = i + 1;

        for (j, b_char) in b_chars.iter().enumerate() {
            let cost = if a_char == b_char { 0 } else { 1 };
            curr_row[j + 1] = std::cmp::min(
                std::cmp::min(
                    prev_row[j + 1] + 1, // deletion
                    curr_row[j] + 1,     // insertion
                ),
                prev_row[j] + cost, // substitution
            );
        }

        std::mem::swap(&mut prev_row, &mut curr_row);
    }

    prev_row[b_len]
}

/// A suggested command with its distance from the input
#[derive(Debug, Clone)]
pub struct CommandSuggestion {
    pub command: String,
    pub distance: usize,
}

/// Find similar commands based on Levenshtein distance
///
/// # Arguments
/// * `typo` - The mistyped command
/// * `paths` - List of PATH directories to search for executables
/// * `builtins` - List of builtin command names
///
/// # Returns
/// A vector of suggestions sorted by distance (closest first), limited to 3 items
pub fn find_similar_commands(
    typo: &str,
    paths: &[String],
    builtins: &[String],
) -> Vec<CommandSuggestion> {
    let typo_len = typo.len();

    // Don't suggest for very short or very long commands
    if !(2..=30).contains(&typo_len) {
        return Vec::new();
    }

    // Threshold: allow distance up to 2 or 30% of input length (whichever is larger)
    let max_distance = std::cmp::max(2, typo_len * 30 / 100);

    let mut seen: HashSet<String> = HashSet::new();
    let mut suggestions: Vec<CommandSuggestion> = Vec::new();

    // Check builtin commands first
    for builtin in builtins {
        if seen.contains(builtin) {
            continue;
        }

        let distance = levenshtein_distance(typo, builtin);
        if distance <= max_distance && distance > 0 {
            seen.insert(builtin.clone());
            suggestions.push(CommandSuggestion {
                command: builtin.clone(),
                distance,
            });
        }
    }

    // Check commands in PATH
    for path in paths {
        if let Ok(entries) = read_dir(path) {
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let Some(name) = file_name.to_str() else {
                    continue;
                };

                if seen.contains(name) {
                    continue;
                }

                // Skip hidden files
                if name.starts_with('.') {
                    continue;
                }

                // Check if it's a file or symlink (potential executable)
                if let Ok(ft) = entry.file_type()
                    && !ft.is_file()
                    && !ft.is_symlink()
                {
                    continue;
                }

                let distance = levenshtein_distance(typo, name);
                if distance <= max_distance && distance > 0 {
                    seen.insert(name.to_string());
                    suggestions.push(CommandSuggestion {
                        command: name.to_string(),
                        distance,
                    });
                }
            }
        }
    }

    // Sort by distance, then alphabetically
    suggestions.sort_by(|a, b| {
        a.distance
            .cmp(&b.distance)
            .then_with(|| a.command.cmp(&b.command))
    });

    // Return top 3 suggestions
    suggestions.truncate(3);
    suggestions
}

/// Format suggestions for display
pub fn format_suggestions(suggestions: &[CommandSuggestion]) -> Option<String> {
    if suggestions.is_empty() {
        return None;
    }

    if suggestions.len() == 1 {
        Some(format!("\rDid you mean: {} ?\r\n", suggestions[0].command))
    } else {
        let commands: Vec<&str> = suggestions.iter().map(|s| s.command.as_str()).collect();
        Some(format!("\rDid you mean: {} ?\r\n", commands.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_distance_same() {
        assert_eq!(levenshtein_distance("hello", "hello"), 0);
    }

    #[test]
    fn test_levenshtein_distance_one_char_diff() {
        assert_eq!(levenshtein_distance("cat", "car"), 1);
        assert_eq!(levenshtein_distance("ls", "lss"), 1);
        assert_eq!(levenshtein_distance("cd", "cdd"), 1);
    }

    #[test]
    fn test_levenshtein_distance_swap() {
        // gti -> git requires 2 operations (swap = delete + insert)
        assert_eq!(levenshtein_distance("gti", "git"), 2);
    }

    #[test]
    fn test_levenshtein_distance_empty() {
        assert_eq!(levenshtein_distance("", "hello"), 5);
        assert_eq!(levenshtein_distance("hello", ""), 5);
        assert_eq!(levenshtein_distance("", ""), 0);
    }

    #[test]
    fn test_levenshtein_distance_completely_different() {
        assert_eq!(levenshtein_distance("abc", "xyz"), 3);
    }

    #[test]
    fn test_find_similar_commands_with_builtins() {
        let paths: Vec<String> = vec![];
        let builtins = vec!["cd".to_string(), "exit".to_string(), "help".to_string()];

        let suggestions = find_similar_commands("cdd", &paths, &builtins);
        assert!(!suggestions.is_empty());
        assert_eq!(suggestions[0].command, "cd");
        assert_eq!(suggestions[0].distance, 1);
    }

    #[test]
    fn test_find_similar_commands_no_match() {
        let paths: Vec<String> = vec![];
        let builtins = vec!["cd".to_string()];

        // Very different command should not match
        let suggestions = find_similar_commands("zzzzzzz", &paths, &builtins);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_find_similar_commands_exact_match_excluded() {
        let paths: Vec<String> = vec![];
        let builtins = vec!["cd".to_string()];

        // Exact match (distance 0) should be excluded
        let suggestions = find_similar_commands("cd", &paths, &builtins);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_format_suggestions_single() {
        let suggestions = vec![CommandSuggestion {
            command: "git".to_string(),
            distance: 2,
        }];
        assert_eq!(
            format_suggestions(&suggestions),
            Some("\rDid you mean: git ?\r\n".to_string())
        );
    }

    #[test]
    fn test_format_suggestions_multiple() {
        let suggestions = vec![
            CommandSuggestion {
                command: "git".to_string(),
                distance: 1,
            },
            CommandSuggestion {
                command: "gist".to_string(),
                distance: 2,
            },
        ];
        assert_eq!(
            format_suggestions(&suggestions),
            Some("\rDid you mean: git, gist ?\r\n".to_string())
        );
    }

    #[test]
    fn test_format_suggestions_empty() {
        let suggestions: Vec<CommandSuggestion> = vec![];
        assert_eq!(format_suggestions(&suggestions), None);
    }

    #[test]
    fn test_short_command_not_suggested() {
        let paths: Vec<String> = vec![];
        let builtins = vec!["cd".to_string()];

        // Too short input should not get suggestions
        let suggestions = find_similar_commands("a", &paths, &builtins);
        assert!(suggestions.is_empty());
    }
}
