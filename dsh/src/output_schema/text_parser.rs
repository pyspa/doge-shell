//! Plain-text output → `Table`, driven by a `TextSpec`.
//!
//! The row/column splitting itself (whitespace runs, fixed-width columns
//! located by header text, and a fixed delimiter) lives in
//! `dsh_types::output_text::split_rows` -- shared with `output-gen --check`,
//! which needs the exact same splitting logic against real captured output
//! but has no use for (and can't depend on) `lisp::model::Table`. This file
//! is the thin adapter that attaches `dsh`'s `Value` types to those fields.
//! Typing is lenient: a cell that fails to parse as its declared type stays
//! a string rather than failing the row.

use crate::lisp::{FloatType, IntType, Record, Table, Value};
use dsh_types::output_schema::{ColumnType, TextSpec};
use dsh_types::output_text::split_rows;

pub fn parse_text(output: &str, spec: &TextSpec) -> Result<Table, String> {
    let rows = split_rows(output, spec)?;

    let mut table = Table::new(
        spec.columns
            .iter()
            .map(|column| column.name.clone())
            .collect(),
    );

    for fields in rows {
        let mut record = Record::new();
        for (column, field) in spec.columns.iter().zip(fields) {
            record.set(column.name.clone(), typed_value(&field, column.column_type));
        }
        table.rows.push(record);
    }

    Ok(table)
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
        ColumnType::Size => dsh_types::output_text::parse_size(field)
            .map(|bytes| Value::Int(IntType::from(bytes)))
            .unwrap_or_else(fallback),
    }
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
    fn declared_header_lines_must_exist() {
        let spec = spec_for("ps", &["aux"]);
        assert!(parse_text("", spec.text.as_ref().unwrap()).is_err());
    }
}
