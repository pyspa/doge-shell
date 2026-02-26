use crate::completion::display::{Candidate, CompletionConfig};
use crate::completion::framework::{CompletionFrameworkKind, CompletionSelection};
use skim::SkimItem;
use std::borrow::Cow;

impl SkimItem for Candidate {
    fn output(&self) -> Cow<'_, str> {
        match self {
            Candidate::Item(text, _) => Cow::Borrowed(text),
            Candidate::Path(path) => Cow::Borrowed(path),
            Candidate::Basic(text) => Cow::Borrowed(text),
            Candidate::Command { name, .. } => Cow::Borrowed(name),
            Candidate::Option { name, .. } => Cow::Borrowed(name),
            Candidate::File { path, .. } => Cow::Borrowed(path),
            Candidate::GitBranch { name, .. } => Cow::Borrowed(name),
            Candidate::History { command, .. } => Cow::Borrowed(command),
            Candidate::Process { pid, .. } => Cow::Borrowed(pid),
        }
    }

    fn text(&self) -> Cow<'_, str> {
        match self {
            Candidate::Item(x, y) => {
                let desc = format!("{x:<30} {y}");
                Cow::Owned(desc)
            }
            Candidate::Path(p) => Cow::Borrowed(p),
            Candidate::Basic(x) => Cow::Borrowed(x),
            Candidate::Command { name, description } => {
                let icon = "⚡"; // Command icon
                if description.is_empty() {
                    Cow::Owned(format!("{icon} {name}"))
                } else {
                    Cow::Owned(format!("{icon} {name:<30} {description}"))
                }
            }
            Candidate::Option { name, description } => {
                let icon = "🔧"; // Option icon
                if description.is_empty() {
                    Cow::Owned(format!("{icon} {name}"))
                } else {
                    Cow::Owned(format!("{icon} {name:<30} {description}"))
                }
            }
            Candidate::File { path, is_dir } => {
                let type_indicator = if *is_dir { "/" } else { "" };
                Cow::Owned(format!("{path}{type_indicator}"))
            }
            Candidate::GitBranch { name, is_current } => {
                let indicator = if *is_current { " (current)" } else { "" };
                Cow::Owned(format!("{name}{indicator}"))
            }
            Candidate::History {
                command, frequency, ..
            } => {
                let desc = format!("{command:<30} used {frequency} times");
                Cow::Owned(desc)
            }
            Candidate::Process { pid, command } => {
                let icon = "🔧";
                let desc = format!("{icon} {pid:<8} {command}");
                Cow::Owned(desc)
            }
        }
    }
}

pub fn select_item_with_skim(items: Vec<Candidate>, query: Option<&str>) -> CompletionSelection {
    let (prompt_text, input_text) = crate::completion::get_prompt_and_input_for_completion();
    crate::completion::select_completion_items_with_framework(
        items,
        query,
        &prompt_text,
        &input_text,
        CompletionConfig::default(),
        CompletionFrameworkKind::Skim,
    )
}

pub fn replace_space(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_whitespace = false;

    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_whitespace {
                out.push('_');
                in_whitespace = true;
            }
        } else {
            out.push(ch);
            in_whitespace = false;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::framework::CompletionSelection;

    #[test]
    fn test_select_item_with_skim_single_candidate() {
        // Test that single candidate is returned directly without UI
        let single_candidate = vec![Candidate::Basic("single_item".to_string())];
        let result = select_item_with_skim(single_candidate, None);
        assert_eq!(
            result,
            CompletionSelection::Selected("single_item".to_string())
        );
    }

    #[test]
    #[ignore] // Ignored because it requires user interaction
    fn test_select_item_with_skim_multiple_candidates() {
        // Test that multiple candidates still require UI selection (would return None in test environment)
        let multiple_candidates = vec![
            Candidate::Basic("first_item".to_string()),
            Candidate::Basic("second_item".to_string()),
        ];
        let _result = select_item_with_skim(multiple_candidates, None);
        // In a test environment without actual UI, this would return None
        // The important thing is that it doesn't immediately return the first item
        // Since we can't easily test the actual UI behavior in unit tests,
        // we rely on the fact that logic will be tested in integration
    }

    #[test]
    fn test_replace_space_basic() {
        assert_eq!(super::replace_space("hello world"), "hello_world");
    }

    #[test]
    fn test_replace_space_multiple_spaces() {
        assert_eq!(super::replace_space("hello   world"), "hello_world");
    }

    #[test]
    fn test_replace_space_tabs_and_newlines() {
        assert_eq!(super::replace_space("hello\tworld\nfoo"), "hello_world_foo");
    }

    #[test]
    fn test_replace_space_no_whitespace() {
        assert_eq!(super::replace_space("hello"), "hello");
    }

    #[test]
    fn test_replace_space_empty() {
        assert_eq!(super::replace_space(""), "");
    }
}
