//! Markdown runbook rendering for `blocks export`.
//!
//! A runbook is a Markdown file that `notebook-play` can execute step by
//! step: each command is one ```sh fence (notebook-play only runs `bash`,
//! `sh`, or unlabeled fences), output excerpts are blockquotes (never
//! executed, and immune to fence injection), and machine-readable metadata
//! rides in HTML comments that terminal renderers do not show.
//!
//! Everything here is pure so the format is testable without a shell.

use dsh_types::ansi::strip_ansi;
use dsh_types::command_block::CommandBlock;
pub use dsh_types::placeholder::Placeholder;

pub struct RunbookOptions {
    pub title: Option<String>,
    /// Output lines quoted per step before the excerpt is elided.
    pub max_excerpt_lines: usize,
    /// Optional per-step descriptions (same order as the blocks), typically
    /// AI-generated. Missing or short vectors simply leave steps bare.
    pub descriptions: Option<Vec<String>>,
}

impl Default for RunbookOptions {
    fn default() -> Self {
        Self {
            title: None,
            max_excerpt_lines: 10,
            descriptions: None,
        }
    }
}

/// Render blocks (oldest first — a runbook is a procedure, so callers must
/// hand in chronological order) as an executable Markdown runbook.
pub fn render_runbook(blocks: &[CommandBlock], opts: &RunbookOptions) -> String {
    let mut out = String::new();
    let title = opts.title.as_deref().unwrap_or("Session export");
    out.push_str(&format!("# Runbook: {}\n\n", sanitize_inline(title)));
    out.push_str(&format!(
        "<!-- dsh:runbook v1 exported={} -->\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));

    for (step, block) in blocks.iter().enumerate() {
        out.push('\n');
        out.push_str(&format!(
            "## Step {}: {}\n\n",
            step + 1,
            step_heading(&block.command)
        ));
        out.push_str(&format!(
            "<!-- dsh:block id={} exit={} duration_ms={}{} -->\n",
            block.id,
            block.exit_code,
            block.duration_ms,
            block
                .cwd
                .as_deref()
                .map(|cwd| format!(" cwd={}", sanitize_inline(cwd)))
                .unwrap_or_default()
        ));
        if block.command.contains('\n') {
            out.push_str(
                "<!-- dsh:warning multi-line command; notebook-play executes line by line -->\n",
            );
        }

        if let Some(description) = opts
            .descriptions
            .as_ref()
            .and_then(|descriptions| descriptions.get(step))
            .map(|description| description.trim())
            .filter(|description| !description.is_empty())
        {
            out.push('\n');
            out.push_str(&sanitize_inline(description));
            out.push('\n');
        }

        out.push('\n');
        let fence = fence_for(&block.command);
        out.push_str(&format!(
            "{fence}sh\n{}\n{fence}\n",
            block.command.trim_end()
        ));

        let excerpt = output_excerpt(block, opts.max_excerpt_lines);
        if !excerpt.is_empty() {
            out.push('\n');
            out.push_str(&excerpt);
        }
    }

    out
}

/// First line of the command, shortened for a heading.
fn step_heading(command: &str) -> String {
    const MAX: usize = 60;
    let first = command.lines().next().unwrap_or("").trim();
    if first.chars().count() > MAX {
        first.chars().take(MAX - 1).collect::<String>() + "…"
    } else {
        first.to_string()
    }
}

/// A fence longer than any backtick run inside the command, so the command
/// itself can never close the fence early.
fn fence_for(command: &str) -> String {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in command.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// Output excerpt as a blockquote: parsed as Markdown by `notebook-play`, so
/// never executed, and safe even if the output contains ``` fences.
fn output_excerpt(block: &CommandBlock, max_lines: usize) -> String {
    let output = if block.stdout.trim().is_empty() {
        block.stderr.as_str()
    } else {
        block.stdout.as_str()
    };
    let cleaned = strip_ansi(output);
    let lines: Vec<&str> = cleaned.lines().collect();
    if lines.iter().all(|line| line.trim().is_empty()) {
        return String::new();
    }

    let shown = lines.len().min(max_lines);
    let mut excerpt = String::new();
    for line in &lines[..shown] {
        excerpt.push_str("> ");
        excerpt.push_str(line.trim_end_matches('\r'));
        excerpt.push('\n');
    }
    if lines.len() > shown {
        excerpt.push_str(&format!("> … (excerpt, {shown}/{} lines)\n", lines.len()));
    }
    excerpt
}

/// Collapse whitespace runs so user-controlled text cannot break out of a
/// heading or an HTML comment.
fn sanitize_inline(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("-->", "→")
}

/// Unique placeholders in order of first appearance; the first default wins.
///
/// Thin wrapper over the shared scanner so runbook playback and snippet
/// expansion cannot disagree about what a placeholder is.
pub fn parse_placeholders(code: &str) -> Vec<Placeholder> {
    dsh_types::placeholder::unique_placeholders(code)
}

/// Replace markers with `values[name]`, falling back to the marker's default;
/// a name with neither stays verbatim so the gap is visible, as does any
/// `{{...}}` that is not a placeholder.
pub fn substitute_placeholders(
    code: &str,
    values: &std::collections::HashMap<String, String>,
) -> String {
    let mut out = String::with_capacity(code.len());
    let mut copied = 0usize;

    for marker in dsh_types::placeholder::markers(code) {
        out.push_str(&code[copied..marker.start]);
        match values
            .get(marker.name)
            .map(String::as_str)
            .or(marker.default)
        {
            Some(replacement) => out.push_str(replacement),
            None => out.push_str(&code[marker.start..marker.end]),
        }
        copied = marker.end;
    }
    out.push_str(&code[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: u64, command: &str, stdout: &str, stderr: &str, exit: i32) -> CommandBlock {
        let mut block = CommandBlock::new(command.into(), Some("/tmp".into()), exit, 5, &[], None);
        block.id = id;
        block.stdout = stdout.to_string();
        block.stderr = stderr.to_string();
        block
    }

    #[test]
    fn renders_commands_in_given_order_with_metadata() {
        let blocks = vec![
            block(1, "cargo build", "Compiling foo\nFinished dev", "", 0),
            block(2, "cargo test", "", "error: oops", 101),
        ];
        let md = render_runbook(&blocks, &RunbookOptions::default());

        assert!(md.starts_with("# Runbook: Session export\n"));
        assert!(md.contains("<!-- dsh:runbook v1 exported="));
        assert!(md.contains("## Step 1: cargo build"));
        assert!(md.contains("<!-- dsh:block id=1 exit=0 duration_ms=5 cwd=/tmp -->"));
        assert!(md.contains("```sh\ncargo build\n```"));
        assert!(md.contains("> Compiling foo\n> Finished dev\n"));
        // stderr is quoted when stdout is empty
        assert!(md.contains("> error: oops\n"));
        let step1 = md.find("## Step 1").unwrap();
        let step2 = md.find("## Step 2").unwrap();
        assert!(step1 < step2);
    }

    #[test]
    fn excerpt_is_truncated_and_annotated() {
        let output = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let blocks = vec![block(1, "seq 20", &output, "", 0)];
        let opts = RunbookOptions {
            max_excerpt_lines: 3,
            ..Default::default()
        };
        let md = render_runbook(&blocks, &opts);
        assert!(md.contains("> line 1\n> line 2\n> line 3\n> … (excerpt, 3/20 lines)\n"));
        assert!(!md.contains("line 4"));
    }

    #[test]
    fn output_with_fences_stays_quoted_not_executable() {
        let blocks = vec![block(1, "cat doc.md", "```sh\nrm -rf /\n```", "", 0)];
        let md = render_runbook(&blocks, &RunbookOptions::default());
        // The dangerous content only appears behind blockquote markers.
        assert!(md.contains("> ```sh\n> rm -rf /\n> ```\n"));
        assert!(!md.contains("\n```sh\nrm -rf /"));
    }

    #[test]
    fn command_with_backticks_gets_a_longer_fence() {
        let blocks = vec![block(1, "echo ```dangerous```", "", "", 0)];
        let md = render_runbook(&blocks, &RunbookOptions::default());
        assert!(md.contains("````sh\necho ```dangerous```\n````"));
    }

    #[test]
    fn multi_line_command_gets_a_warning() {
        let blocks = vec![block(1, "echo one\necho two", "", "", 0)];
        let md = render_runbook(&blocks, &RunbookOptions::default());
        assert!(md.contains("<!-- dsh:warning multi-line command"));
    }

    #[test]
    fn ansi_sequences_are_stripped_from_excerpts() {
        let blocks = vec![block(
            1,
            "ls --color",
            "\u{1b}[31mred.txt\u{1b}[0m\r",
            "",
            0,
        )];
        let md = render_runbook(&blocks, &RunbookOptions::default());
        assert!(md.contains("> red.txt\n"));
        assert!(!md.contains('\u{1b}'));
    }

    #[test]
    fn descriptions_are_inserted_per_step() {
        let blocks = vec![
            block(1, "make", "", "", 0),
            block(2, "make test", "", "", 0),
        ];
        let opts = RunbookOptions {
            descriptions: Some(vec!["Build everything.".to_string()]),
            ..Default::default()
        };
        let md = render_runbook(&blocks, &opts);
        assert!(md.contains("Build everything.\n\n```sh\nmake\n```"));
        // Second step has no description and still renders.
        assert!(md.contains("## Step 2: make test"));
    }

    #[test]
    fn placeholders_are_parsed_in_order_and_deduplicated() {
        let placeholders =
            parse_placeholders("scp {{src}} {{host:localhost}}:{{dst}} && echo {{src}}");
        assert_eq!(
            placeholders,
            vec![
                Placeholder {
                    name: "src".into(),
                    default: None
                },
                Placeholder {
                    name: "host".into(),
                    default: Some("localhost".into())
                },
                Placeholder {
                    name: "dst".into(),
                    default: None
                },
            ]
        );
        assert!(parse_placeholders("echo {{oops").is_empty());
    }

    #[test]
    fn go_template_syntax_is_not_a_placeholder() {
        // The docker output schema injects exactly this format string, so an
        // exported runbook will contain it.
        for code in [
            "docker ps --format '{{json .}}'",
            "docker inspect --format '{{.State.Status}}' web",
            "helm template --set x={{ .Values.name }}",
            "echo {{}}",
            "echo {{2fast}}",
        ] {
            assert!(
                parse_placeholders(code).is_empty(),
                "{code} should have no placeholders"
            );
            let values = std::collections::HashMap::from([
                ("json .".to_string(), "boom".to_string()),
                (".State.Status".to_string(), "boom".to_string()),
            ]);
            assert_eq!(
                substitute_placeholders(code, &values),
                code,
                "{code} must survive substitution unchanged"
            );
        }
    }

    #[test]
    fn placeholder_substitution_uses_values_then_defaults() {
        let mut values = std::collections::HashMap::new();
        values.insert("src".to_string(), "a.txt".to_string());
        let code = "scp {{src}} {{host:localhost}}:{{dst}}";
        assert_eq!(
            substitute_placeholders(code, &values),
            "scp a.txt localhost:{{dst}}"
        );
        assert_eq!(
            substitute_placeholders("echo {{oops", &values),
            "echo {{oops"
        );
    }

    #[test]
    fn title_and_cwd_cannot_break_the_comment() {
        let mut b = block(1, "ls", "", "", 0);
        b.cwd = Some("/tmp/x --> y".to_string());
        let opts = RunbookOptions {
            title: Some("multi\nline\ntitle".to_string()),
            ..Default::default()
        };
        let md = render_runbook(&[b], &opts);
        assert!(md.contains("# Runbook: multi line title"));
        assert!(md.contains("cwd=/tmp/x → y -->"));
    }
}
