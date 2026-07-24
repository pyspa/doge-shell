use crate::completion::display::{Candidate, CompletionConfig};
use crate::completion::framework::{CompletionFrameworkKind, CompletionSelection};
use crate::completion::subprocess::shell_quote;
use skim::{ItemPreview, PreviewContext, SkimItem};
use std::borrow::Cow;

/// What to show in the skim preview pane for a candidate. Split out from the
/// `SkimItem::preview` impl so the command construction is unit-testable
/// (`ItemPreview` is not comparable).
#[derive(Debug, PartialEq)]
pub(crate) enum PreviewSpec {
    /// A read-only shell command whose output skim renders (async, cancellable).
    Command(String),
    /// Static text (used for candidates whose "preview" is their description).
    Text(String),
    /// Nothing meaningful to preview.
    None,
}

/// Build a context-appropriate preview for a completion candidate. All commands
/// are read-only and embed shell-quoted values directly (no `{}` placeholder),
/// so pathological candidate text cannot inject shell commands.
pub(crate) fn candidate_preview(candidate: &Candidate) -> PreviewSpec {
    match candidate {
        Candidate::File { path, is_dir } => file_preview(path, *is_dir),
        Candidate::Path(path) => {
            let is_dir = path.ends_with('/');
            let trimmed = path.strip_suffix('/').unwrap_or(path);
            file_preview(trimmed, is_dir)
        }
        Candidate::GitBranch { name, .. } => PreviewSpec::Command(format!(
            "git log --oneline --color=always --max-count=20 {} 2>/dev/null",
            shell_quote(name)
        )),
        Candidate::Process { pid, .. } => {
            if !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) {
                PreviewSpec::Command(format!(
                    "ps -p {pid} -o pid,ppid,pgid,stat,etime,pcpu,pmem,args 2>/dev/null"
                ))
            } else {
                PreviewSpec::None
            }
        }
        Candidate::Command { description, .. }
        | Candidate::Option { description, .. }
        | Candidate::Item(_, description) => {
            if description.is_empty() {
                PreviewSpec::None
            } else {
                PreviewSpec::Text(description.clone())
            }
        }
        Candidate::Basic(_) | Candidate::History { .. } => PreviewSpec::None,
    }
}

fn file_preview(path: &str, is_dir: bool) -> PreviewSpec {
    let quoted = shell_quote(path);
    if is_dir {
        PreviewSpec::Command(format!("ls -la -- {quoted} 2>/dev/null"))
    } else {
        PreviewSpec::Command(format!(
            "bat --color=always --style=plain -- {quoted} 2>/dev/null || cat -- {quoted} 2>/dev/null"
        ))
    }
}

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

    fn preview(&self, _context: PreviewContext) -> ItemPreview {
        match candidate_preview(self) {
            PreviewSpec::Command(cmd) => ItemPreview::Command(cmd),
            PreviewSpec::Text(text) => ItemPreview::Text(text),
            PreviewSpec::None => ItemPreview::Global,
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
    fn preview_for_file_uses_bat_with_quoted_path() {
        let spec = candidate_preview(&Candidate::File {
            path: "src/main.rs".to_string(),
            is_dir: false,
        });
        assert_eq!(
            spec,
            PreviewSpec::Command(
                "bat --color=always --style=plain -- 'src/main.rs' 2>/dev/null \
                 || cat -- 'src/main.rs' 2>/dev/null"
                    .to_string()
            )
        );
    }

    #[test]
    fn preview_for_directory_lists_contents() {
        let dir = candidate_preview(&Candidate::File {
            path: "target".to_string(),
            is_dir: true,
        });
        assert_eq!(
            dir,
            PreviewSpec::Command("ls -la -- 'target' 2>/dev/null".to_string())
        );
        // A Path ending in `/` is treated as a directory too.
        let path_dir = candidate_preview(&Candidate::Path("target/".to_string()));
        assert_eq!(
            path_dir,
            PreviewSpec::Command("ls -la -- 'target' 2>/dev/null".to_string())
        );
    }

    #[test]
    fn preview_for_git_branch_shows_log() {
        let spec = candidate_preview(&Candidate::GitBranch {
            name: "feature/x".to_string(),
            is_current: false,
        });
        assert_eq!(
            spec,
            PreviewSpec::Command(
                "git log --oneline --color=always --max-count=20 'feature/x' 2>/dev/null"
                    .to_string()
            )
        );
    }

    #[test]
    fn preview_for_process_requires_numeric_pid() {
        let ok = candidate_preview(&Candidate::Process {
            pid: "1234".to_string(),
            command: "sleep".to_string(),
        });
        assert!(matches!(ok, PreviewSpec::Command(_)));
        // A non-numeric pid must not be spliced into a command.
        let bad = candidate_preview(&Candidate::Process {
            pid: "12; rm -rf ~".to_string(),
            command: "x".to_string(),
        });
        assert_eq!(bad, PreviewSpec::None);
    }

    #[test]
    fn preview_for_command_uses_description_text() {
        let spec = candidate_preview(&Candidate::Command {
            name: "ls".to_string(),
            description: "list directory contents".to_string(),
        });
        assert_eq!(
            spec,
            PreviewSpec::Text("list directory contents".to_string())
        );
        // Empty description => nothing to preview.
        let empty = candidate_preview(&Candidate::Command {
            name: "ls".to_string(),
            description: String::new(),
        });
        assert_eq!(empty, PreviewSpec::None);
    }

    #[test]
    fn preview_quotes_paths_with_shell_metacharacters() {
        let spec = candidate_preview(&Candidate::File {
            path: "a'; rm -rf ~".to_string(),
            is_dir: false,
        });
        // The dangerous path must be single-quoted (with the embedded quote escaped),
        // never left bare.
        match spec {
            PreviewSpec::Command(cmd) => {
                assert!(cmd.contains("'a'\\''; rm -rf ~'"), "unquoted path in: {cmd}");
            }
            other => panic!("expected command preview, got {other:?}"),
        }
    }

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
