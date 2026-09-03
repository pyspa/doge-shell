//! Pure text-splitting for `TextSpec`-described command output: string in,
//! string fields out, no table type attached.
//!
//! This is the row/column-splitting half of what used to live entirely in
//! `dsh`'s `output_schema::text_parser` (whitespace runs, fixed-width
//! columns located by header text, and delimited fields). It moved here so
//! `output-gen`'s `--check` can run the exact same splitting logic `|:` uses
//! at runtime against a real captured sample, without `dsh-builtin` needing
//! to depend on `dsh` for a `lisp::model::Table` it has no use for (crates
//! can't depend on each other that way; see `invariants.md`'s "two crates,
//! one pure implementation" rule). `dsh`'s `text_parser.rs` calls
//! [`split_rows`] and then attaches its own `Value` types to build a `Table`;
//! [`looks_like_type`] answers the narrower question `output-gen --check`
//! actually needs -- does this column's data look like its declared type --
//! without needing a `Value` at all.

use crate::output_schema::{ColumnSpec, ColumnType, NamedSeparator, Separator, TextSpec};

/// Splits `output` into per-row string fields, one `Vec<String>` per data
/// row, aligned with `spec.columns` in order (a line with fewer fields than
/// columns yields a short `Vec` -- callers zip against `spec.columns` and
/// tolerate that already).
pub fn split_rows(output: &str, spec: &TextSpec) -> Result<Vec<Vec<String>>, String> {
    if spec.columns.is_empty() {
        return Err("text spec has no columns".to_string());
    }

    let lines = filtered_lines(output, spec);
    if lines.len() < spec.header_lines {
        return Err(format!(
            "output has {} lines but the schema declares {} header line(s)",
            lines.len(),
            spec.header_lines
        ));
    }
    let (header_lines, data_lines) = lines.split_at(spec.header_lines);

    let boundaries = match &spec.separator {
        Separator::Named(NamedSeparator::Auto) => {
            let header = header_lines
                .last()
                .ok_or_else(|| "auto separator requires a header line".to_string())?;
            Some(auto_boundaries(header, &spec.columns)?)
        }
        _ => None,
    };

    let mut rows = Vec::with_capacity(data_lines.len());
    for line in data_lines {
        let fields = match &spec.separator {
            Separator::Named(NamedSeparator::Whitespace) => {
                split_whitespace_fields(line, &spec.columns)
            }
            Separator::Named(NamedSeparator::Auto) => {
                split_fixed(line, boundaries.as_deref().unwrap_or(&[]))
            }
            Separator::Delimiter { delimiter } => split_delimited(line, delimiter, &spec.columns),
        };
        if fields.iter().all(|field| field.is_empty()) {
            continue;
        }
        rows.push(fields);
    }
    Ok(rows)
}

fn filtered_lines<'a>(output: &'a str, spec: &TextSpec) -> Vec<&'a str> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| {
            !spec
                .skip_prefixes
                .iter()
                .any(|prefix| line.starts_with(prefix))
        })
        .collect()
}

/// How many data lines (after blank/`skip_prefixes` filtering and setting
/// aside `header_lines`) `split_rows` would have to work with -- *before*
/// per-line field splitting drops any that come out all-empty. Lets a
/// caller tell "the command's output was genuinely empty" (this is 0 too)
/// apart from "every data line failed to split into non-empty fields" (this
/// is >0 while `split_rows` still returns zero rows), which `split_rows`'s
/// return value alone can't distinguish.
pub fn count_data_lines(output: &str, spec: &TextSpec) -> Result<usize, String> {
    let lines = filtered_lines(output, spec);
    if lines.len() < spec.header_lines {
        return Err(format!(
            "output has {} lines but the schema declares {} header line(s)",
            lines.len(),
            spec.header_lines
        ));
    }
    Ok(lines.len() - spec.header_lines)
}

/// Split on whitespace runs into at most `columns.len()` fields; a final
/// `rest` column takes the remainder of the line verbatim (trimmed).
fn split_whitespace_fields(line: &str, columns: &[ColumnSpec]) -> Vec<String> {
    let take_rest = columns.last().is_some_and(|column| column.rest);
    let max_fields = columns.len();
    let mut fields = Vec::with_capacity(max_fields);
    let mut rest = line.trim_start();

    while !rest.is_empty() && fields.len() < max_fields {
        if take_rest && fields.len() == max_fields - 1 {
            fields.push(rest.trim_end().to_string());
            return fields;
        }
        match rest.find(char::is_whitespace) {
            Some(end) => {
                fields.push(rest[..end].to_string());
                rest = rest[end..].trim_start();
            }
            None => {
                fields.push(rest.to_string());
                rest = "";
            }
        }
    }
    fields
}

fn split_delimited(line: &str, delimiter: &str, columns: &[ColumnSpec]) -> Vec<String> {
    let take_rest = columns.last().is_some_and(|column| column.rest);
    if take_rest {
        line.splitn(columns.len(), delimiter)
            .map(|field| field.trim().to_string())
            .collect()
    } else {
        line.split(delimiter)
            .take(columns.len())
            .map(|field| field.trim().to_string())
            .collect()
    }
}

/// Column start offsets (in chars) from the header text positions.
fn auto_boundaries(header: &str, columns: &[ColumnSpec]) -> Result<Vec<usize>, String> {
    let header_chars: Vec<char> = header.chars().collect();
    let mut boundaries = Vec::with_capacity(columns.len());
    let mut search_from = 0usize;

    for column in columns {
        let label = column
            .header
            .clone()
            .unwrap_or_else(|| column.name.to_uppercase());
        let label_chars: Vec<char> = label.chars().collect();
        let position = find_chars(&header_chars, &label_chars, search_from)
            .ok_or_else(|| format!("header label {label:?} not found in header line {header:?}"))?;
        boundaries.push(position);
        search_from = position + label_chars.len();
    }
    Ok(boundaries)
}

fn find_chars(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len())
        .find(|&start| haystack[start..start + needle.len()] == *needle)
}

/// Slice a line by fixed char boundaries; each field is trimmed.
fn split_fixed(line: &str, boundaries: &[usize]) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut fields = Vec::with_capacity(boundaries.len());
    for (index, &start) in boundaries.iter().enumerate() {
        let end = boundaries
            .get(index + 1)
            .copied()
            .unwrap_or(chars.len())
            .min(chars.len());
        let start = start.min(chars.len());
        let field: String = chars[start..end].iter().collect();
        fields.push(field.trim().to_string());
    }
    fields
}

/// Whether `field` would parse cleanly as `column_type`. `String`/
/// `Duration`/`Date` are untyped passthroughs and always match; the rest
/// mirror `dsh`'s `text_parser::typed_value` fallback rule exactly (a cell
/// that fails to parse as its declared type stays a string rather than
/// failing the row), so this reports precisely the cells that rule would
/// have to fall back on.
pub fn looks_like_type(field: &str, column_type: ColumnType) -> bool {
    match column_type {
        ColumnType::String | ColumnType::Duration | ColumnType::Date => true,
        ColumnType::Int => field.parse::<i64>().is_ok(),
        ColumnType::Float => field.parse::<f64>().is_ok(),
        ColumnType::Percent => field.trim_end_matches('%').parse::<f64>().is_ok(),
        ColumnType::Size => parse_size(field).is_some(),
    }
}

/// `1024`, `1.5K`, `2Gi`, `3MB` → bytes. `-` (ls for directories on some
/// systems, df placeholders) fails and stays a string.
pub fn parse_size(field: &str) -> Option<i64> {
    let trimmed = field.trim();
    if trimmed.is_empty() {
        return None;
    }
    let unit_start = trimmed
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.' && ch != ',')
        .unwrap_or(trimmed.len());
    let (number, unit) = trimmed.split_at(unit_start);
    let number: f64 = number.replace(',', "").parse().ok()?;

    let multiplier: f64 = match unit
        .trim()
        .trim_end_matches(['i', 'I', 'b', 'B'])
        .to_ascii_uppercase()
        .as_str()
    {
        "" => 1.0,
        "K" => 1024.0,
        "M" => 1024.0 * 1024.0,
        "G" => 1024.0_f64.powi(3),
        "T" => 1024.0_f64.powi(4),
        "P" => 1024.0_f64.powi(5),
        _ => return None,
    };
    Some((number * multiplier).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_schema::{NamedSeparator, OutputSchema};

    fn column(name: &str, header: Option<&str>, column_type: ColumnType, rest: bool) -> ColumnSpec {
        ColumnSpec {
            name: name.to_string(),
            header: header.map(str::to_string),
            column_type,
            rest,
        }
    }

    #[test]
    fn splits_whitespace_rows_and_keeps_rest_column_greedy() {
        let spec = TextSpec {
            separator: Separator::Named(NamedSeparator::Whitespace),
            header_lines: 1,
            skip_prefixes: Vec::new(),
            columns: vec![
                column("user", None, ColumnType::String, false),
                column("pid", None, ColumnType::Int, false),
                column("command", None, ColumnType::String, true),
            ],
        };
        let output = "USER PID COMMAND\nroot 1 /sbin/init --deserialize 43\n";
        let rows = split_rows(output, &spec).unwrap();
        assert_eq!(rows, vec![vec!["root", "1", "/sbin/init --deserialize 43"]]);
    }

    #[test]
    fn errors_when_declared_header_lines_are_missing() {
        let spec = TextSpec {
            separator: Separator::Named(NamedSeparator::Whitespace),
            header_lines: 1,
            skip_prefixes: Vec::new(),
            columns: vec![column("a", None, ColumnType::String, false)],
        };
        assert!(split_rows("", &spec).is_err());
    }

    #[test]
    fn count_data_lines_distinguishes_no_data_from_unsplittable_data() {
        let spec = TextSpec {
            separator: Separator::Named(NamedSeparator::Whitespace),
            header_lines: 1,
            skip_prefixes: Vec::new(),
            columns: vec![column("a", None, ColumnType::String, false)],
        };
        // Header only: genuinely no data.
        assert_eq!(count_data_lines("USER\n", &spec).unwrap(), 0);
        // A data line is present and split_rows finds it.
        assert_eq!(count_data_lines("USER\nroot\n", &spec).unwrap(), 1);
        assert_eq!(split_rows("USER\nroot\n", &spec).unwrap().len(), 1);
    }

    #[test]
    fn auto_separator_locates_columns_by_header_text() {
        let spec = TextSpec {
            separator: Separator::Named(NamedSeparator::Auto),
            header_lines: 1,
            skip_prefixes: Vec::new(),
            columns: vec![
                column(
                    "container_id",
                    Some("CONTAINER ID"),
                    ColumnType::String,
                    false,
                ),
                column("image", Some("IMAGE"), ColumnType::String, false),
                column("names", Some("NAMES"), ColumnType::String, true),
            ],
        };
        let output =
            "CONTAINER ID   IMAGE          NAMES\n1a2b3c4d5e6f   nginx:latest   web server\n";
        let rows = split_rows(output, &spec).unwrap();
        assert_eq!(rows[0][0], "1a2b3c4d5e6f");
        assert_eq!(rows[0][1], "nginx:latest");
        assert_eq!(rows[0][2], "web server");
    }

    #[test]
    fn delimited_rest_column_keeps_embedded_delimiters() {
        let spec = TextSpec {
            separator: Separator::Delimiter {
                delimiter: "\t".to_string(),
            },
            header_lines: 0,
            skip_prefixes: Vec::new(),
            columns: vec![
                column("hash", None, ColumnType::String, false),
                column("subject", None, ColumnType::String, true),
            ],
        };
        let rows = split_rows("abc\tfix: a\tb\n", &spec).unwrap();
        assert_eq!(rows, vec![vec!["abc", "fix: a\tb"]]);
    }

    #[test]
    fn looks_like_type_matches_typed_value_fallback_rule() {
        assert!(looks_like_type("42", ColumnType::Int));
        assert!(!looks_like_type("abc", ColumnType::Int));
        assert!(looks_like_type("3.5", ColumnType::Float));
        assert!(looks_like_type("42%", ColumnType::Percent));
        assert!(looks_like_type("1.5K", ColumnType::Size));
        assert!(!looks_like_type("-", ColumnType::Size));
        assert!(looks_like_type("anything at all", ColumnType::String));
        assert!(looks_like_type("anything at all", ColumnType::Duration));
        assert!(looks_like_type("anything at all", ColumnType::Date));
    }

    #[test]
    fn size_suffix_parsing() {
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size("1.5K"), Some(1536));
        assert_eq!(parse_size("2Gi"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("3MB"), Some(3 * 1024 * 1024));
        assert_eq!(parse_size("-"), None);
        assert_eq!(parse_size(""), None);
    }

    /// Both `split_rows` and `looks_like_type` operate on any valid
    /// `OutputSchema`, not just ones built by hand in these tests -- a
    /// smoke test against the embedded ps.json spec shape.
    #[test]
    fn works_against_a_realistic_schema_shape() {
        let schema: OutputSchema = serde_json::from_str(
            r#"{"command":"ps","outputs":[{"when":{"args_include":["aux"]},
               "text":{"separator":"whitespace","header_lines":1,"columns":[
               {"name":"user"},{"name":"pid","type":"int"},
               {"name":"command","rest":true}]}}]}"#,
        )
        .unwrap();
        let spec = &schema.outputs[0];
        let text = spec.text.as_ref().unwrap();
        let rows = split_rows("USER PID COMMAND\nroot 1 init\n", text).unwrap();
        assert_eq!(rows, vec![vec!["root", "1", "init"]]);
        assert!(looks_like_type(&rows[0][1], text.columns[1].column_type));
    }
}
