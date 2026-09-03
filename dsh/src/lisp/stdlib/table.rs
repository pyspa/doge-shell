use crate::lisp::model::{
    CmpValue, Env, IntType, List, RuntimeError, Symbol, Table, TableRc, Value,
};
use crate::lisp::utils::{require_arg, require_typed_arg};
use cfg_if::cfg_if;
use std::cell::RefCell;
use std::convert::TryInto;

/// Fetches a `TableRc` argument at `index`, with a friendlier error than the
/// generic "requires argument N to be a table" when it's a plain string --
/// the shape `$_` has when the command has no matching `output-schemas/*.json`
/// entry (`|:` falls back to the raw text rather than fail the pipeline).
/// Every other mismatch still gets the generic message.
fn require_table<'a>(
    func_name: &str,
    args: &'a [Value],
    index: usize,
) -> Result<&'a TableRc, RuntimeError> {
    match args.get(index) {
        Some(Value::String(_)) => Err(RuntimeError {
            msg: format!(
                "\"{func_name}\": expected a table but got a plain string. If this came from \
                 `|:`, the command likely has no output-schema yet -- run \
                 `output-gen \"<command>\"` to generate one. Otherwise, parse the string \
                 explicitly with `json-parse`/`csv-parse`/`output-parse` first."
            ),
        }),
        _ => require_typed_arg::<&TableRc>(func_name, args, index),
    }
}

/// Resolves `column` against `table`'s schema (exact match, then
/// case-insensitive), erroring with the available column names when neither
/// matches. Column-taking functions used to fail silently on a typo'd or
/// differently-cased name (an empty result, not an error).
fn resolve_column<'a>(
    func_name: &str,
    table: &'a Table,
    column: &str,
) -> Result<&'a str, RuntimeError> {
    table.resolve_column(column).ok_or_else(|| RuntimeError {
        msg: format!(
            "\"{func_name}\": no column named '{column}' (available: {})",
            table.columns.join(", ")
        ),
    })
}

pub fn register(env: &mut Env) {
    // dsh-struct-pipe-error: raise a runtime error carrying the given
    // message. Internal: `dsh/src/shell/parse.rs` desugars a malformed `|:`
    // DSL stage into `(dsh-struct-pipe-error "...")` instead of failing the
    // whole parse, so a typo in one `;`-joined command's `|:` doesn't
    // discard jobs already parsed for the commands before it -- the bad
    // stage just fails at its own run time, through the same "Struct pipe
    // error: ..." path any other struct-pipe failure already goes through.
    env.define(
        Symbol::from("dsh-struct-pipe-error"),
        Value::NativeFunc(|_env, args| {
            let msg = require_typed_arg::<&String>("dsh-struct-pipe-error", &args, 0)?;
            Err(RuntimeError { msg: msg.clone() })
        }),
    );

    // json-parse: Parse JSON string into a Table
    env.define(
        Symbol::from("json-parse"),
        Value::NativeFunc(|_env, args| {
            let json_str = require_typed_arg::<&String>("json-parse", &args, 0)?;

            match Table::from_json(json_str) {
                Ok(table) => Ok(Value::Table(TableRc::new(RefCell::new(table)))),
                Err(e) => Err(RuntimeError {
                    msg: format!("json-parse error: {}", e),
                }),
            }
        }),
    );

    // json-stringify: Convert a Table to JSON string
    env.define(
        Symbol::from("json-stringify"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("json-stringify", &args, 0)?;
            let table = table_rc.borrow();
            Ok(Value::String(table.to_json()))
        }),
    );

    // output-parse: Parse command output text using a declarative output
    // schema (output-schemas/*.json), the same lookup `|:` applies
    // automatically. Usage: (output-parse "ps aux" text)
    env.define(
        Symbol::from("output-parse"),
        Value::NativeFunc(|_env, args| {
            let command_line = require_typed_arg::<&String>("output-parse", &args, 0)?;
            let text = require_typed_arg::<&String>("output-parse", &args, 1)?;

            let argv: Vec<String> = command_line
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let Some(spec) = crate::output_schema::lookup(&argv) else {
                return Err(RuntimeError {
                    msg: format!("output-parse: no output schema matches {command_line:?}"),
                });
            };
            match crate::output_schema::parse_with_spec(&spec, text) {
                Ok(table) => Ok(Value::Table(TableRc::new(RefCell::new(table)))),
                Err(e) => Err(RuntimeError {
                    msg: format!("output-parse error: {}", e),
                }),
            }
        }),
    );

    // csv-parse: Parse CSV string into a Table
    env.define(
        Symbol::from("csv-parse"),
        Value::NativeFunc(|_env, args| {
            let csv_str = require_typed_arg::<&String>("csv-parse", &args, 0)?;

            match Table::from_csv(csv_str) {
                Ok(table) => Ok(Value::Table(TableRc::new(RefCell::new(table)))),
                Err(e) => Err(RuntimeError {
                    msg: format!("csv-parse error: {}", e),
                }),
            }
        }),
    );

    // csv-stringify: Convert a Table to CSV string
    env.define(
        Symbol::from("csv-stringify"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("csv-stringify", &args, 0)?;
            let table = table_rc.borrow();
            match table.to_csv() {
                Ok(s) => Ok(Value::String(s)),
                Err(e) => Err(RuntimeError {
                    msg: format!("csv-stringify error: {}", e),
                }),
            }
        }),
    );

    // table-select: Select specific columns from a table
    env.define(
        Symbol::from("table-select"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-select", &args, 0)?;
            let columns_list = require_typed_arg::<&List>("table-select", &args, 1)?;

            let requested: Vec<String> = columns_list
                .into_iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s),
                    Value::Symbol(s) => Some(s.0.clone()),
                    _ => None,
                })
                .collect();

            let table = table_rc.borrow();
            let columns = requested
                .iter()
                .map(|c| resolve_column("table-select", &table, c).map(str::to_string))
                .collect::<Result<Vec<_>, _>>()?;
            let col_refs: Vec<&str> = columns.iter().map(|s| s.as_str()).collect();
            let new_table = table.select(&col_refs);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-head: Get first n rows from a table
    env.define(
        Symbol::from("table-head"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-head", &args, 0)?;
            let n = require_typed_arg::<IntType>("table-head", &args, 1)?;
            let n: usize = n.try_into().map_err(|_| RuntimeError {
                msg: "table-head: n must be a non-negative integer".to_string(),
            })?;

            let table = table_rc.borrow();
            let new_table = table.head(n);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-tail: Get last n rows from a table
    env.define(
        Symbol::from("table-tail"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-tail", &args, 0)?;
            let n = require_typed_arg::<IntType>("table-tail", &args, 1)?;
            let n: usize = n.try_into().map_err(|_| RuntimeError {
                msg: "table-tail: n must be a non-negative integer".to_string(),
            })?;

            let table = table_rc.borrow();
            let new_table = table.tail(n);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // is_table: Check if a value is a table
    env.define(
        Symbol::from("is_table"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_table", &args, 0)?;
            Ok(match val {
                Value::Table(_) => Value::True,
                _ => Value::NIL,
            })
        }),
    );

    // table-to-ai-context: Format table for AI prompts with schema and sample data
    env.define(
        Symbol::from("table-to-ai-context"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-to-ai-context", &args, 0)?;
            let max_rows = if args.len() > 1 {
                let n = require_typed_arg::<IntType>("table-to-ai-context", &args, 1)?;
                n.try_into().unwrap_or(5)
            } else {
                5 // Default to 5 sample rows
            };

            let table = table_rc.borrow();
            Ok(Value::String(table.to_ai_context(max_rows)))
        }),
    );

    // table-display: Display table in formatted output
    env.define(
        Symbol::from("table-display"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-display", &args, 0)?;
            let table = table_rc.borrow();
            Ok(Value::String(table.to_display()))
        }),
    );

    // table-count: Count rows in a table
    env.define(
        Symbol::from("table-count"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-count", &args, 0)?;
            let table = table_rc.borrow();
            let count = table.count();
            cfg_if! {
                if #[cfg(feature = "bigint")] {
                    Ok(Value::Int(IntType::from(count)))
                } else {
                    Ok(Value::Int(count as IntType))
                }
            }
        }),
    );

    // table-where-eq: Filter rows where column equals value
    // Usage: (table-where-eq table "column" value)
    env.define(
        Symbol::from("table-where-eq"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-where-eq", &args, 0)?;
            let column = require_typed_arg::<&String>("table-where-eq", &args, 1)?;
            let value = require_arg("table-where-eq", &args, 2)?;

            let table = table_rc.borrow();
            let column = resolve_column("table-where-eq", &table, column)?;
            let new_table = table.where_eq(column, value);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-where-contains: Filter rows where string column contains substring
    // Usage: (table-where-contains table "column" "substring")
    env.define(
        Symbol::from("table-where-contains"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-where-contains", &args, 0)?;
            let column = require_typed_arg::<&String>("table-where-contains", &args, 1)?;
            let substring = require_typed_arg::<&String>("table-where-contains", &args, 2)?;

            let table = table_rc.borrow();
            let column = resolve_column("table-where-contains", &table, column)?;
            let new_table = table.where_contains(column, substring);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-where-cmp: Filter rows with numeric comparison
    // Usage: (table-where-cmp table "column" ">" 10)
    env.define(
        Symbol::from("table-where-cmp"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-where-cmp", &args, 0)?;
            let column = require_typed_arg::<&String>("table-where-cmp", &args, 1)?;
            let op = require_typed_arg::<&String>("table-where-cmp", &args, 2)?;
            // Int or Float: `%CPU` style columns hold floats, while ids and
            // byte counts need exact integer comparison.
            let value = match args.get(3).and_then(CmpValue::from_value) {
                Some(value) => value,
                None => {
                    return Err(RuntimeError {
                        msg: format!(
                            "table-where-cmp requires a numeric value argument, got {:?}",
                            args.get(3)
                        ),
                    });
                }
            };

            let table = table_rc.borrow();
            let column = resolve_column("table-where-cmp", &table, column)?;
            let new_table = table.where_cmp(column, op, &value);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-order-by: Sort table by column
    // Usage: (table-order-by table "column") or (table-order-by table "column" :desc)
    env.define(
        Symbol::from("table-order-by"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-order-by", &args, 0)?;
            let column = require_typed_arg::<&String>("table-order-by", &args, 1)?;
            let ascending = if args.len() > 2 {
                match &args[2] {
                    Value::Symbol(s) if s.0 == ":desc" || s.0 == "desc" => false,
                    Value::String(s) if s == "desc" || s == ":desc" => false,
                    _ => true,
                }
            } else {
                true
            };

            let table = table_rc.borrow();
            let column = resolve_column("table-order-by", &table, column)?;
            let new_table = table.order_by(column, ascending);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-where-ne: Filter rows where column does not equal value
    // Usage: (table-where-ne table "column" value)
    env.define(
        Symbol::from("table-where-ne"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-where-ne", &args, 0)?;
            let column = require_typed_arg::<&String>("table-where-ne", &args, 1)?;
            let value = require_arg("table-where-ne", &args, 2)?;

            let table = table_rc.borrow();
            let column = resolve_column("table-where-ne", &table, column)?;
            let new_table = table.where_ne(column, value);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-distinct: Keep the first row for each distinct value of a column
    // Usage: (table-distinct table "column")
    env.define(
        Symbol::from("table-distinct"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-distinct", &args, 0)?;
            let column = require_typed_arg::<&String>("table-distinct", &args, 1)?;

            let table = table_rc.borrow();
            let column = resolve_column("table-distinct", &table, column)?;
            let new_table = table.distinct(column);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-rename: Rename a column
    // Usage: (table-rename table "old" "new")
    env.define(
        Symbol::from("table-rename"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-rename", &args, 0)?;
            let old = require_typed_arg::<&String>("table-rename", &args, 1)?;
            let new = require_typed_arg::<&String>("table-rename", &args, 2)?;

            let table = table_rc.borrow();
            let old = resolve_column("table-rename", &table, old)?;
            let new_table = table.rename(old, new).map_err(|msg| RuntimeError {
                msg: format!("\"table-rename\": {msg}"),
            })?;
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-group-by: Group rows by a column, counting each distinct value
    // Usage: (table-group-by table "column") -> table with [column, count]
    env.define(
        Symbol::from("table-group-by"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-group-by", &args, 0)?;
            let column = require_typed_arg::<&String>("table-group-by", &args, 1)?;

            let table = table_rc.borrow();
            let column = resolve_column("table-group-by", &table, column)?;
            let new_table = table.group_by(column);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-count-by: table-group-by, ordered by count descending
    // Usage: (table-count-by table "column")
    env.define(
        Symbol::from("table-count-by"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-count-by", &args, 0)?;
            let column = require_typed_arg::<&String>("table-count-by", &args, 1)?;

            let table = table_rc.borrow();
            let column = resolve_column("table-count-by", &table, column)?;
            let new_table = table.count_by(column);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-sum: Sum every numeric cell in a column
    // Usage: (table-sum table "column")
    env.define(
        Symbol::from("table-sum"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-sum", &args, 0)?;
            let column = require_typed_arg::<&String>("table-sum", &args, 1)?;

            let table = table_rc.borrow();
            let column = resolve_column("table-sum", &table, column)?;
            table.sum(column).ok_or_else(|| RuntimeError {
                msg: format!("\"table-sum\": no numeric values in column '{column}'"),
            })
        }),
    );

    // table-avg: Average every numeric cell in a column
    // Usage: (table-avg table "column")
    env.define(
        Symbol::from("table-avg"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-avg", &args, 0)?;
            let column = require_typed_arg::<&String>("table-avg", &args, 1)?;

            let table = table_rc.borrow();
            let column = resolve_column("table-avg", &table, column)?;
            match table.avg(column) {
                Some(avg) => Ok(Value::Float(avg)),
                None => Err(RuntimeError {
                    msg: format!("\"table-avg\": no numeric values in column '{column}'"),
                }),
            }
        }),
    );

    // table-min: The smallest numeric cell in a column
    // Usage: (table-min table "column")
    env.define(
        Symbol::from("table-min"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-min", &args, 0)?;
            let column = require_typed_arg::<&String>("table-min", &args, 1)?;

            let table = table_rc.borrow();
            let column = resolve_column("table-min", &table, column)?;
            table.min(column).ok_or_else(|| RuntimeError {
                msg: format!("\"table-min\": no numeric values in column '{column}'"),
            })
        }),
    );

    // table-max: The largest numeric cell in a column
    // Usage: (table-max table "column")
    env.define(
        Symbol::from("table-max"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_table("table-max", &args, 0)?;
            let column = require_typed_arg::<&String>("table-max", &args, 1)?;

            let table = table_rc.borrow();
            let column = resolve_column("table-max", &table, column)?;
            table.max(column).ok_or_else(|| RuntimeError {
                msg: format!("\"table-max\": no numeric values in column '{column}'"),
            })
        }),
    );
}
