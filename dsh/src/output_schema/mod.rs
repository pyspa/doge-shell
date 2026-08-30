//! Declarative output schemas for the `|:` structured pipe.
//!
//! `output-schemas/*.json` (embedded, user-overridable) describe how known
//! commands lay out their stdout. When a `|:` pipeline's last external
//! command matches a schema, its captured output is parsed into a
//! `Value::Table` and bound to `$_` (the raw text stays in `$RAW`), so
//! `ps aux |: (table-where-cmp $_ "cpu" ">" 50)` works without hand-written
//! parsing. Parsing failures fall back to the plain string silently — a
//! schema must never break command execution.

pub mod loader;
pub mod text_parser;

pub use loader::lookup;

use crate::lisp::Table;
use dsh_types::output_schema::{OutputSpec, ParseMode};

/// Parse captured output with a matched spec.
///
/// A `prefer` mode that fails to parse falls back to the `text` spec when one
/// exists (e.g. the injected `--format` was ignored by an older tool).
pub fn parse_with_spec(spec: &OutputSpec, output: &str) -> Result<Table, String> {
    if let Some(prefer) = &spec.prefer {
        let parsed = match prefer.parse {
            ParseMode::Json => parse_json(output, prefer.json_root.as_deref()),
            ParseMode::JsonLines => parse_json_lines(output),
            ParseMode::Text => Err("prefer uses the text spec".to_string()),
        };
        match parsed {
            Ok(table) => return Ok(table),
            Err(err) => {
                if spec.text.is_none() {
                    return Err(err);
                }
            }
        }
    }

    let text = spec
        .text
        .as_ref()
        .ok_or_else(|| "schema has no text spec".to_string())?;
    text_parser::parse_text(output, text)
}

fn parse_json(output: &str, json_root: Option<&str>) -> Result<Table, String> {
    let value: serde_json::Value =
        serde_json::from_str(output).map_err(|e| format!("json parse: {e}"))?;
    let value = match json_root {
        Some(root) => value.get(root).cloned().unwrap_or(value),
        None => value,
    };
    Table::from_json_value(&value)
}

fn parse_json_lines(output: &str) -> Result<Table, String> {
    let objects = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("json-lines parse: {e}"))?;
    if objects.is_empty() {
        return Ok(Table::empty());
    }
    Table::from_json_value(&serde_json::Value::Array(objects))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::output_schema::PreferSpec;

    fn json_spec(parse: ParseMode, json_root: Option<&str>) -> OutputSpec {
        OutputSpec {
            subcommand: None,
            when: None,
            prefer: Some(PreferSpec {
                inject_args: vec![],
                parse,
                json_root: json_root.map(str::to_string),
            }),
            text: None,
        }
    }

    #[test]
    fn json_lines_parse_into_a_table() {
        let spec = json_spec(ParseMode::JsonLines, None);
        let output =
            "{\"Names\":\"web\",\"Status\":\"Up\"}\n{\"Names\":\"db\",\"Status\":\"Exited\"}\n";
        let table = parse_with_spec(&spec, output).unwrap();
        assert_eq!(table.columns, vec!["Names", "Status"]);
        assert_eq!(table.rows.len(), 2);
    }

    #[test]
    fn json_root_unwraps_the_items_key() {
        let spec = json_spec(ParseMode::Json, Some("items"));
        let output = r#"{"kind":"List","items":[{"name":"a"},{"name":"b"}]}"#;
        let table = parse_with_spec(&spec, output).unwrap();
        assert_eq!(table.rows.len(), 2);
    }

    #[test]
    fn failed_prefer_parse_falls_back_to_text_spec() {
        use dsh_types::output_schema::{ColumnSpec, ColumnType, TextSpec};
        let mut spec = json_spec(ParseMode::JsonLines, None);
        spec.text = Some(TextSpec {
            separator: Default::default(),
            header_lines: 0,
            skip_prefixes: vec![],
            columns: vec![ColumnSpec {
                name: "word".to_string(),
                header: None,
                column_type: ColumnType::String,
                rest: true,
            }],
        });
        let table = parse_with_spec(&spec, "not json\n").unwrap();
        assert_eq!(table.rows.len(), 1);
    }
}
