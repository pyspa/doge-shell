//! Plain-text output → `Table`, driven by a `TextSpec`.
//!
//! Three splitting strategies: whitespace runs (ps, df), fixed-width columns
//! located by header text positions (docker ps), and a fixed delimiter
//! (injected `--pretty=format:` TSV). Typing is lenient: a cell that fails to
//! parse as its declared type stays a string rather than failing the row.

use crate::lisp::{FloatType, IntType, Record, Table, Value};
use dsh_types::output_schema::{ColumnSpec, ColumnType, NamedSeparator, Separator, TextSpec};

pub fn parse_text(output: &str, spec: &TextSpec) -> Result<Table, String> {
    if spec.columns.is_empty() {
        return Err("text spec has no columns".to_string());
    }

    let lines: Vec<&str> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| {
            !spec
                .skip_prefixes
                .iter()
                .any(|prefix| line.starts_with(prefix))
        })
        .collect();

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

    let mut table = Table::new(
        spec.columns
            .iter()
            .map(|column| column.name.clone())
            .collect(),
    );

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

        let mut record = Record::new();
        for (column, field) in spec.columns.iter().zip(fields) {
            record.set(column.name.clone(), typed_value(&field, column.column_type));
        }
        table.rows.push(record);
    }

    Ok(table)
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

fn typed_value(field: &str, column_type: ColumnType) -> Value {
    let fallback = || Value::String(field.to_string());
    match column_type {
        ColumnType::String | ColumnType::Duration | ColumnType::Date => fallback(),
        ColumnType::Int => field
            .parse::<i64>()
            .map(|n| Value::Int(IntType::from(n)))
            .unwrap_or_else(|_| fallback()),
        ColumnType::Float => field
            .parse::<FloatType>()
            .map(Value::Float)
            .unwrap_or_else(|_| fallback()),
        ColumnType::Percent => field
            .trim_end_matches('%')
            .parse::<FloatType>()
            .map(Value::Float)
            .unwrap_or_else(|_| fallback()),
        ColumnType::Size => parse_size(field)
            .map(|bytes| Value::Int(IntType::from(bytes)))
            .unwrap_or_else(fallback),
    }
}

/// `1024`, `1.5K`, `2Gi`, `3MB` → bytes. `-` (ls for directories on some
/// systems, df placeholders) fails and stays a string.
fn parse_size(field: &str) -> Option<i64> {
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
    use dsh_types::output_schema::OutputSchema;

    fn spec_for(command: &str, args: &[&str]) -> dsh_types::output_schema::OutputSpec {
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../output-schemas")
            .join(format!("{command}.json"));
        let schema: OutputSchema =
            serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        schema
            .outputs
            .iter()
            .find(|spec| spec.matches(&args))
            .cloned()
            .expect("spec should match")
    }

    fn get<'a>(table: &'a Table, row: usize, column: &str) -> &'a Value {
        table.rows[row].get(column).expect("cell should exist")
    }

    #[test]
    fn ps_aux_parses_with_typed_cpu_and_greedy_command() {
        let spec = spec_for("ps", &["aux"]);
        let output = "\
USER         PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
root           1  0.5  0.1  22568 13060 ?        Ss   Aug29   0:03 /sbin/init --deserialize 43
ma2         4242 55.0  2.0 123456 65536 pts/1    R+   10:00   1:23 cargo test -p doge-shell
";
        let table = parse_text(output, spec.text.as_ref().unwrap()).unwrap();
        assert_eq!(table.rows.len(), 2);
        assert_eq!(get(&table, 0, "user"), &Value::String("root".into()));
        assert_eq!(get(&table, 1, "cpu"), &Value::Float(55.0));
        assert_eq!(
            get(&table, 1, "command"),
            &Value::String("cargo test -p doge-shell".into())
        );
        assert_eq!(get(&table, 0, "pid"), &Value::Int(IntType::from(1i64)));
    }

    #[test]
    fn ls_l_skips_total_and_parses_sizes() {
        let spec = spec_for("ls", &["-la"]);
        let output = "\
total 16
drwxr-xr-x 2 ma2 users 4096 Aug 30 10:00 src
-rw-r--r-- 1 ma2 users 1.5K Aug 30 09:59 read me.md
";
        let table = parse_text(output, spec.text.as_ref().unwrap()).unwrap();
        assert_eq!(table.rows.len(), 2);
        assert_eq!(get(&table, 0, "size"), &Value::Int(IntType::from(4096i64)));
        assert_eq!(get(&table, 1, "size"), &Value::Int(IntType::from(1536i64)));
        // Spaces in the file name survive via the rest column.
        assert_eq!(get(&table, 1, "name"), &Value::String("read me.md".into()));
    }

    #[test]
    fn docker_ps_fixed_width_columns_from_header_positions() {
        let spec = spec_for("docker", &["ps"]);
        let output = "\
CONTAINER ID   IMAGE          COMMAND                  CREATED        STATUS        PORTS                  NAMES
1a2b3c4d5e6f   nginx:latest   \"/docker-entrypoint.…\"   2 hours ago    Up 2 hours    0.0.0.0:8080->80/tcp   web server
";
        let table = parse_text(output, spec.text.as_ref().unwrap()).unwrap();
        assert_eq!(table.rows.len(), 1);
        assert_eq!(
            get(&table, 0, "container_id"),
            &Value::String("1a2b3c4d5e6f".into())
        );
        assert_eq!(
            get(&table, 0, "image"),
            &Value::String("nginx:latest".into())
        );
        // Empty PORTS-like gaps trim cleanly; NAMES takes the rest.
        assert_eq!(get(&table, 0, "names"), &Value::String("web server".into()));
    }

    #[test]
    fn git_log_tsv_via_delimiter() {
        let spec = spec_for("git", &["log"]);
        let output = "abc123\tAlice\t2026-08-30\tfeat: add runbook export\n\
def456\tBob\t2026-08-29\tfix: subject with\ttab inside\n";
        let table = parse_text(output, spec.text.as_ref().unwrap()).unwrap();
        assert_eq!(table.rows.len(), 2);
        assert_eq!(get(&table, 0, "hash"), &Value::String("abc123".into()));
        // The rest column keeps embedded delimiters.
        assert_eq!(
            get(&table, 1, "subject"),
            &Value::String("fix: subject with\ttab inside".into())
        );
    }

    #[test]
    fn unparsable_typed_cells_stay_strings() {
        let spec = spec_for("df", &["-h"]);
        let output = "\
Filesystem      Size  Used Avail Use% Mounted on
/dev/sda1        50G   20G   28G  42% /
tmpfs              -     -     -    - /dev/shm
";
        let table = parse_text(output, spec.text.as_ref().unwrap()).unwrap();
        assert_eq!(
            get(&table, 0, "size"),
            &Value::Int(IntType::from(50i64 * 1024 * 1024 * 1024))
        );
        assert_eq!(get(&table, 0, "use_percent"), &Value::Float(42.0));
        assert_eq!(get(&table, 1, "size"), &Value::String("-".into()));
        assert_eq!(get(&table, 0, "mounted"), &Value::String("/".into()));
    }

    #[test]
    fn plain_df_reports_1k_blocks_not_bytes() {
        // Unsuffixed `df` prints 1K-blocks; typing them as bytes would be off
        // by 1024, so the column is named and typed for what it is.
        let spec = spec_for("df", &[]);
        let output = "\
Filesystem     1K-blocks      Used Available Use% Mounted on
/dev/sda1      514531328 305977528 187614696  62% /
";
        let table = parse_text(output, spec.text.as_ref().unwrap()).unwrap();
        assert_eq!(
            get(&table, 0, "avail_1k"),
            &Value::Int(IntType::from(187614696i64))
        );
        assert_eq!(get(&table, 0, "use_percent"), &Value::Float(62.0));
    }

    #[test]
    fn free_w_gets_its_own_eight_column_variant() {
        // `free -w` splits buff/cache into two columns; the 7-column spec
        // would shift `cache` into `available`.
        let spec = spec_for("free", &["-w"]);
        let output = "\
               total        used        free      shared     buffers       cache   available
Mem:        32574560     8123456    12000000      500000      300000    12151104    23000000
";
        let table = parse_text(output, spec.text.as_ref().unwrap()).unwrap();
        assert_eq!(
            get(&table, 0, "cache_kib"),
            &Value::Int(IntType::from(12151104i64))
        );
        assert_eq!(
            get(&table, 0, "available_kib"),
            &Value::Int(IntType::from(23000000i64))
        );
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

    #[test]
    fn declared_header_lines_must_exist() {
        let spec = spec_for("ps", &["aux"]);
        assert!(parse_text("", spec.text.as_ref().unwrap()).is_err());
    }
}
