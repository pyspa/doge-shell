use crate::lisp::model::{Env, IntType, List, RuntimeError, Symbol, Table, TableRc, Value};
use crate::lisp::utils::{require_arg, require_typed_arg};
use cfg_if::cfg_if;
use std::cell::RefCell;
use std::convert::TryInto;

pub fn register(env: &mut Env) {
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
            let table_rc = require_typed_arg::<&TableRc>("json-stringify", &args, 0)?;
            let table = table_rc.borrow();
            Ok(Value::String(table.to_json()))
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
            let table_rc = require_typed_arg::<&TableRc>("csv-stringify", &args, 0)?;
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
            let table_rc = require_typed_arg::<&TableRc>("table-select", &args, 0)?;
            let columns_list = require_typed_arg::<&List>("table-select", &args, 1)?;

            let columns: Vec<String> = columns_list
                .into_iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s),
                    Value::Symbol(s) => Some(s.0.clone()),
                    _ => None,
                })
                .collect();
            let col_refs: Vec<&str> = columns.iter().map(|s| s.as_str()).collect();

            let table = table_rc.borrow();
            let new_table = table.select(&col_refs);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-head: Get first n rows from a table
    env.define(
        Symbol::from("table-head"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_typed_arg::<&TableRc>("table-head", &args, 0)?;
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
            let table_rc = require_typed_arg::<&TableRc>("table-tail", &args, 0)?;
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
            let table_rc = require_typed_arg::<&TableRc>("table-to-ai-context", &args, 0)?;
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
            let table_rc = require_typed_arg::<&TableRc>("table-display", &args, 0)?;
            let table = table_rc.borrow();
            Ok(Value::String(table.to_display()))
        }),
    );

    // table-count: Count rows in a table
    env.define(
        Symbol::from("table-count"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_typed_arg::<&TableRc>("table-count", &args, 0)?;
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
            let table_rc = require_typed_arg::<&TableRc>("table-where-eq", &args, 0)?;
            let column = require_typed_arg::<&String>("table-where-eq", &args, 1)?;
            let value = require_arg("table-where-eq", &args, 2)?;

            let table = table_rc.borrow();
            let new_table = table.where_eq(column, value);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-where-contains: Filter rows where string column contains substring
    // Usage: (table-where-contains table "column" "substring")
    env.define(
        Symbol::from("table-where-contains"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_typed_arg::<&TableRc>("table-where-contains", &args, 0)?;
            let column = require_typed_arg::<&String>("table-where-contains", &args, 1)?;
            let substring = require_typed_arg::<&String>("table-where-contains", &args, 2)?;

            let table = table_rc.borrow();
            let new_table = table.where_contains(column, substring);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-where-cmp: Filter rows with numeric comparison
    // Usage: (table-where-cmp table "column" ">" 10)
    env.define(
        Symbol::from("table-where-cmp"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_typed_arg::<&TableRc>("table-where-cmp", &args, 0)?;
            let column = require_typed_arg::<&String>("table-where-cmp", &args, 1)?;
            let op = require_typed_arg::<&String>("table-where-cmp", &args, 2)?;
            let value = require_typed_arg::<IntType>("table-where-cmp", &args, 3)?;

            let table = table_rc.borrow();
            let new_table = table.where_cmp(column, op, value);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );

    // table-order-by: Sort table by column
    // Usage: (table-order-by table "column") or (table-order-by table "column" :desc)
    env.define(
        Symbol::from("table-order-by"),
        Value::NativeFunc(|_env, args| {
            let table_rc = require_typed_arg::<&TableRc>("table-order-by", &args, 0)?;
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
            let new_table = table.order_by(column, ascending);
            Ok(Value::Table(TableRc::new(RefCell::new(new_table))))
        }),
    );
}
