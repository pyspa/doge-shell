//! Declarative output schemas.
//!
//! Mirrors the completion JSON approach (`completions/` + embedded assets):
//! `output-schemas/*.json` describe how well-known commands lay out their
//! stdout, so the `|:` structured pipe can turn `ps aux` or `docker ps` into
//! a typed table without hand-written Lisp parsing. The meta schema lives in
//! `command-output-schema.json` at the repository root.

use serde::{Deserialize, Serialize};

/// One schema file: a command plus one spec per output shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputSchema {
    /// Command name (e.g. "ps", "docker").
    pub command: String,
    /// Candidate specs; the first whose `subcommand`/`when` match wins.
    pub outputs: Vec<OutputSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputSpec {
    /// Required subcommand (first non-option argument), e.g. "ps" for
    /// `docker ps`. Absent for commands without subcommands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subcommand: Option<String>,
    /// Extra argv conditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<WhenSpec>,
    /// Machine-readable mode: inject arguments (e.g. `--format {{json .}}`)
    /// and parse that instead of the human text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer: Option<PreferSpec>,
    /// How to parse the plain text output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextSpec>,
}

/// Argv matcher. `args_include` must all be present; `args_exclude` must all
/// be absent. A single-letter short option such as `-l` also matches inside a
/// combined cluster (`-la`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WhenSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args_include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args_exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreferSpec {
    /// Arguments appended to the command before it runs.
    pub inject_args: Vec<String>,
    /// How to parse the resulting output.
    pub parse: ParseMode,
    /// For `parse: "json"`: unwrap this top-level key first (e.g. "items"
    /// for `kubectl get -o json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_root: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ParseMode {
    /// One JSON document.
    Json,
    /// One JSON object per line (`docker --format '{{json .}}'`).
    JsonLines,
    /// Parse with the spec's `text` definition (e.g. an injected
    /// `--pretty=format:` making the text shape deterministic).
    Text,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextSpec {
    #[serde(default)]
    pub separator: Separator,
    /// Header lines to skip (the last one drives `auto` column detection).
    #[serde(default = "default_header_lines")]
    pub header_lines: usize,
    /// Skip lines starting with any of these (e.g. `ls -l`'s "total ").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_prefixes: Vec<String>,
    pub columns: Vec<ColumnSpec>,
}

fn default_header_lines() -> usize {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Separator {
    Named(NamedSeparator),
    /// Fixed delimiter, e.g. `{"delimiter": "\t"}`.
    Delimiter {
        delimiter: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NamedSeparator {
    /// Split on whitespace runs; a trailing `rest` column takes the remainder.
    Whitespace,
    /// Fixed-width columns located by the header text positions
    /// (`docker ps`-style output).
    Auto,
}

impl Default for Separator {
    fn default() -> Self {
        Separator::Named(NamedSeparator::Whitespace)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnSpec {
    /// Column name in the resulting table.
    pub name: String,
    /// Header text when it differs from the name (e.g. "%CPU", "CONTAINER ID").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default, rename = "type")]
    pub column_type: ColumnType,
    /// This (final) column greedily takes the rest of the line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rest: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ColumnType {
    #[default]
    String,
    Int,
    Float,
    /// `3.5%` or `3.5` → float 3.5.
    Percent,
    /// Human sizes (`1.5K`, `2Gi`, plain bytes) → integer bytes.
    Size,
    /// Kept as string for now; a typed representation can come later.
    Duration,
    /// Kept as string for now.
    Date,
}

impl WhenSpec {
    /// Match against the arguments after the command name.
    pub fn matches(&self, args: &[String]) -> bool {
        self.args_include.iter().all(|want| arg_present(args, want))
            && !self.args_exclude.iter().any(|ban| arg_present(args, ban))
    }
}

fn arg_present(args: &[String], wanted: &str) -> bool {
    args.iter().any(|arg| {
        if arg == wanted {
            return true;
        }
        // "--pretty" also matches "--pretty=oneline".
        if wanted.starts_with("--")
            && arg.starts_with(wanted)
            && arg[wanted.len()..].starts_with('=')
        {
            return true;
        }
        // "-l" also matches a combined short cluster like "-la".
        if let Some(letter) = wanted
            .strip_prefix('-')
            .filter(|rest| rest.len() == 1 && !wanted.starts_with("--"))
        {
            return arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains(letter);
        }
        false
    })
}

impl OutputSpec {
    /// Whether this spec applies to the given arguments (after the command
    /// name). The subcommand, when required, must be the first non-option
    /// argument.
    pub fn matches(&self, args: &[String]) -> bool {
        if let Some(subcommand) = &self.subcommand {
            let first = args.iter().find(|arg| !arg.starts_with('-'));
            if first.map(String::as_str) != Some(subcommand.as_str()) {
                return false;
            }
        }
        self.when.as_ref().is_none_or(|when| when.matches(args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn schema_round_trips_through_serde() {
        let json = r#"{
            "command": "docker",
            "outputs": [{
                "subcommand": "ps",
                "when": { "args_exclude": ["-q", "--format"] },
                "prefer": { "inject_args": ["--format", "{{json .}}"], "parse": "json-lines" },
                "text": {
                    "separator": "auto",
                    "columns": [
                        { "name": "container_id", "header": "CONTAINER ID" },
                        { "name": "names", "rest": true }
                    ]
                }
            }]
        }"#;
        let schema: OutputSchema = serde_json::from_str(json).unwrap();
        assert_eq!(schema.command, "docker");
        let spec = &schema.outputs[0];
        assert_eq!(spec.prefer.as_ref().unwrap().parse, ParseMode::JsonLines);
        let text = spec.text.as_ref().unwrap();
        assert_eq!(text.separator, Separator::Named(NamedSeparator::Auto));
        assert_eq!(text.header_lines, 1);
        assert!(text.columns[1].rest);

        let back = serde_json::to_string(&schema).unwrap();
        let again: OutputSchema = serde_json::from_str(&back).unwrap();
        assert_eq!(schema, again);
    }

    #[test]
    fn delimiter_separator_parses() {
        let json = r#"{ "separator": { "delimiter": "\t" }, "header_lines": 0, "columns": [{ "name": "a" }] }"#;
        let text: TextSpec = serde_json::from_str(json).unwrap();
        assert_eq!(
            text.separator,
            Separator::Delimiter {
                delimiter: "\t".to_string()
            }
        );
        assert_eq!(text.header_lines, 0);
    }

    #[test]
    fn when_matches_combined_short_options() {
        let when = WhenSpec {
            args_include: vec!["-l".to_string()],
            args_exclude: vec![],
        };
        assert!(when.matches(&args(&["-l"])));
        assert!(when.matches(&args(&["-la"])));
        assert!(when.matches(&args(&["-ltr", "src"])));
        assert!(!when.matches(&args(&["--long"])));
        assert!(!when.matches(&args(&["src"])));

        let exclude = WhenSpec {
            args_include: vec![],
            args_exclude: vec!["--format".to_string()],
        };
        assert!(exclude.matches(&args(&["ps"])));
        assert!(!exclude.matches(&args(&["ps", "--format", "x"])));
    }

    #[test]
    fn spec_subcommand_is_the_first_non_option_argument() {
        let spec = OutputSpec {
            subcommand: Some("ps".to_string()),
            when: None,
            prefer: None,
            text: None,
        };
        assert!(spec.matches(&args(&["ps"])));
        assert!(spec.matches(&args(&["-a", "ps"])));
        assert!(!spec.matches(&args(&["images"])));
        assert!(!spec.matches(&args(&[])));
    }
}
