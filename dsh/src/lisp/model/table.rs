//! Structured table data type for structured data pipelines.
//!
//! This module provides `Table` and `Record` types for handling structured data
//! like JSON objects and arrays in a tabular format.

use cfg_if::cfg_if;
use indexmap::IndexMap;
use serde_json::{self, Value as JsonValue};
use std::cell::RefCell;
use std::fmt::{self, Display};
use std::rc::Rc;

use super::{IntType, Value};

/// A single row (record) in a table.
/// Uses IndexMap to preserve insertion order of fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub fields: IndexMap<String, Value>,
}

impl Record {
    /// Creates a new empty record.
    pub fn new() -> Self {
        Self {
            fields: IndexMap::new(),
        }
    }

    /// Gets a value by key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }

    /// Sets a value for a key.
    pub fn set(&mut self, key: String, value: Value) {
        self.fields.insert(key, value);
    }

    /// Returns the number of fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Returns true if the record has no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Returns an iterator over field names.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.fields.keys()
    }

    /// Converts this record to a JSON object string.
    pub fn to_json(&self) -> String {
        let obj: serde_json::Map<String, JsonValue> = self
            .fields
            .iter()
            .map(|(k, v)| (k.clone(), value_to_json(v)))
            .collect();
        serde_json::to_string(&JsonValue::Object(obj)).unwrap_or_default()
    }
}

impl Default for Record {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{")?;
        let mut first = true;
        for (k, v) in &self.fields {
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "{k}: {v}")?;
            first = false;
        }
        write!(f, "}}")
    }
}

/// A table is a list of records with optional column schema.
#[derive(Debug, Clone)]
pub struct Table {
    /// Column names (in order).
    pub columns: Vec<String>,
    /// Data rows.
    pub rows: Vec<Record>,
}

/// Reference-counted table for use in Value enum.
pub type TableRc = Rc<RefCell<Table>>;

/// Right-hand side of a [`Table::where_cmp`] comparison.
///
/// Keeping the integer case separate preserves exact comparison for values
/// past a float's precision (ids, byte counts) instead of rounding both sides
/// to the nearest representable float.
#[derive(Debug, Clone, PartialEq)]
pub enum CmpValue {
    Int(super::IntType),
    Float(super::FloatType),
}

impl CmpValue {
    /// The comparison value behind a numeric `Value`, or `None` for anything
    /// else.
    // `IntType` is `BigInt` under the `bigint` feature, where this clone is
    // real work rather than a copy.
    #[allow(clippy::clone_on_copy)]
    pub fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Int(n) => Some(CmpValue::Int(n.clone())),
            Value::Float(f) => Some(CmpValue::Float(*f)),
            _ => None,
        }
    }
}

impl Table {
    /// Creates a new empty table with the given columns.
    pub fn new(columns: Vec<String>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    /// Creates a new empty table with no columns.
    pub fn empty() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// Parses a JSON string into a Table.
    ///
    /// Supports:
    /// - JSON array of objects: `[{...}, {...}]`
    /// - Single JSON object: `{...}`
    /// - JSON array of primitives: `[1, 2, 3]` (creates single "value" column)
    pub fn from_json(json: &str) -> Result<Self, String> {
        let parsed: JsonValue =
            serde_json::from_str(json).map_err(|e| format!("JSON parse error: {e}"))?;

        Self::from_json_value(&parsed)
    }

    /// Converts a serde_json Value into a Table.
    pub fn from_json_value(value: &JsonValue) -> Result<Self, String> {
        match value {
            JsonValue::Array(arr) => {
                if arr.is_empty() {
                    return Ok(Self::empty());
                }

                // Check if array of objects
                if arr.iter().all(|v| v.is_object()) {
                    Self::from_json_objects(arr)
                } else {
                    // Array of primitives - create single "value" column
                    let mut table = Self::new(vec!["value".to_string()]);
                    for item in arr {
                        let mut record = Record::new();
                        record.set("value".to_string(), json_to_value(item));
                        table.rows.push(record);
                    }
                    Ok(table)
                }
            }
            JsonValue::Object(_) => {
                // Single object - treat as single-row table
                let table = Self::from_json_objects(std::slice::from_ref(value))?;
                Ok(table)
            }
            _ => {
                // Primitive value - single cell table
                let mut table = Self::new(vec!["value".to_string()]);
                let mut record = Record::new();
                record.set("value".to_string(), json_to_value(value));
                table.rows.push(record);
                Ok(table)
            }
        }
    }

    /// Creates a table from an array of JSON objects.
    fn from_json_objects(objects: &[JsonValue]) -> Result<Self, String> {
        // Collect all unique column names (preserving order of first appearance)
        let mut columns: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        for obj in objects {
            if let JsonValue::Object(map) = obj {
                for key in map.keys() {
                    if !seen.contains(key) {
                        seen.insert(key.clone());
                        columns.push(key.clone());
                    }
                }
            }
        }

        let mut table = Self::new(columns);

        for obj in objects {
            if let JsonValue::Object(map) = obj {
                let mut record = Record::new();
                for (key, value) in map {
                    record.set(key.clone(), json_to_value(value));
                }
                table.rows.push(record);
            }
        }

        Ok(table)
    }

    /// Creates a table from a CSV string.
    pub fn from_csv(csv_str: &str) -> Result<Self, String> {
        let mut rdr = csv::Reader::from_reader(csv_str.as_bytes());
        let headers = rdr.headers().map_err(|e| e.to_string())?.clone();

        let columns: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
        let mut table = Self::new(columns.clone());

        for result in rdr.records() {
            let record = result.map_err(|e| e.to_string())?;
            let mut row = Record::new();

            for (i, field) in record.iter().enumerate() {
                if i < columns.len() {
                    let col_name = &columns[i];
                    let val = if let Ok(n) = field.parse::<super::IntType>() {
                        Value::Int(n)
                    } else if let Ok(f) = field.parse::<f64>() {
                        Value::Float(f)
                    } else {
                        Value::String(field.to_string())
                    };
                    row.set(col_name.clone(), val);
                }
            }
            table.rows.push(row);
        }

        Ok(table)
    }

    /// Converts the table to a CSV string.
    pub fn to_csv(&self) -> Result<String, String> {
        let mut wtr = csv::Writer::from_writer(vec![]);

        // Write headers
        wtr.write_record(&self.columns).map_err(|e| e.to_string())?;

        for row in &self.rows {
            let record: Vec<String> = self
                .columns
                .iter()
                .map(|col| {
                    if let Some(val) = row.get(col) {
                        match val {
                            Value::String(s) => s.clone(),
                            _ => format!("{}", val),
                        }
                    } else {
                        String::new()
                    }
                })
                .collect();
            wtr.write_record(&record).map_err(|e| e.to_string())?;
        }

        let data = String::from_utf8(wtr.into_inner().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        Ok(data)
    }

    /// Returns the number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Returns true if the table has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Adds a row to the table.
    pub fn push(&mut self, record: Record) {
        // Update columns if record has new fields
        for key in record.keys() {
            if !self.columns.contains(key) {
                self.columns.push(key.clone());
            }
        }
        self.rows.push(record);
    }

    /// Converts the table to a JSON array string.
    pub fn to_json(&self) -> String {
        let arr: Vec<JsonValue> = self
            .rows
            .iter()
            .map(|r| {
                let obj: serde_json::Map<String, JsonValue> = r
                    .fields
                    .iter()
                    .map(|(k, v)| (k.clone(), value_to_json(v)))
                    .collect();
                JsonValue::Object(obj)
            })
            .collect();
        serde_json::to_string(&arr).unwrap_or_default()
    }

    /// Converts the table to a display string using tabled.
    pub fn to_display(&self) -> String {
        use tabled::{builder::Builder, settings::Style};

        if self.rows.is_empty() {
            return "(empty table)".to_string();
        }

        let mut builder = Builder::default();

        // Add header
        builder.push_record(&self.columns);

        // Add rows
        for record in &self.rows {
            let row: Vec<String> = self
                .columns
                .iter()
                .map(|col| record.get(col).map(|v| format!("{v}")).unwrap_or_default())
                .collect();
            builder.push_record(row);
        }

        let mut table = builder.build();
        table.with(Style::rounded()).to_string()
    }

    /// Selects specific columns from the table.
    pub fn select(&self, columns: &[&str]) -> Self {
        let selected_columns: Vec<String> = columns.iter().map(|s| s.to_string()).collect();
        let mut new_table = Self::new(selected_columns.clone());

        for record in &self.rows {
            let mut new_record = Record::new();
            for col in &selected_columns {
                if let Some(value) = record.get(col) {
                    new_record.set(col.clone(), value.clone());
                }
            }
            new_table.rows.push(new_record);
        }

        new_table
    }

    /// Returns the first n rows.
    pub fn head(&self, n: usize) -> Self {
        let mut new_table = Self::new(self.columns.clone());
        new_table.rows = self.rows.iter().take(n).cloned().collect();
        new_table
    }

    /// Returns the last n rows.
    pub fn tail(&self, n: usize) -> Self {
        let mut new_table = Self::new(self.columns.clone());
        let len = self.rows.len();
        let start = len.saturating_sub(n);
        new_table.rows = self.rows.iter().skip(start).cloned().collect();
        new_table
    }

    /// Formats the table for AI context, including schema and sample data.
    /// This is optimized to provide useful information while minimizing tokens.
    pub fn to_ai_context(&self, max_sample_rows: usize) -> String {
        use std::fmt::Write;
        let mut output = String::new();

        // Schema information
        writeln!(output, "Table Schema:").ok();
        writeln!(output, "  Columns: {}", self.columns.join(", ")).ok();
        writeln!(output, "  Total Rows: {}", self.rows.len()).ok();

        if self.rows.is_empty() {
            writeln!(output, "  (no data)").ok();
            return output;
        }

        // Sample data (first n rows as JSON for clarity)
        let sample_count = self.rows.len().min(max_sample_rows);
        writeln!(output, "\nSample Data ({} rows):", sample_count).ok();

        for (i, record) in self.rows.iter().take(sample_count).enumerate() {
            let json = record.to_json();
            writeln!(output, "  [{}] {}", i + 1, json).ok();
        }

        if self.rows.len() > sample_count {
            writeln!(
                output,
                "  ... and {} more rows",
                self.rows.len() - sample_count
            )
            .ok();
        }

        output
    }

    /// Counts the number of rows in the table.
    pub fn count(&self) -> usize {
        self.rows.len()
    }

    /// Filters rows where the specified column matches the given value.
    /// For simple equality filtering.
    pub fn where_eq(&self, column: &str, value: &Value) -> Self {
        let mut new_table = Self::new(self.columns.clone());
        for record in &self.rows {
            if let Some(field_value) = record.get(column)
                && field_value == value
            {
                new_table.rows.push(record.clone());
            }
        }
        new_table
    }

    /// Filters rows where the specified column contains the given substring (for string values).
    pub fn where_contains(&self, column: &str, substring: &str) -> Self {
        let mut new_table = Self::new(self.columns.clone());
        for record in &self.rows {
            if let Some(Value::String(s)) = record.get(column)
                && s.contains(substring)
            {
                new_table.rows.push(record.clone());
            }
        }
        new_table
    }

    /// Filters rows where the numeric column matches a comparison.
    /// op can be: ">" "<" ">=" "<=" "=" "!="
    ///
    /// Both `Int` and `Float` cells participate (a `%CPU` of `3.5` must be
    /// comparable); non-numeric cells never match. Two integers are compared
    /// as integers, so ids and byte counts past 2^53 stay exact.
    pub fn where_cmp(&self, column: &str, op: &str, value: &CmpValue) -> Self {
        let mut new_table = Self::new(self.columns.clone());
        for record in &self.rows {
            let ordering = match (record.get(column), value) {
                (Some(Value::Int(cell)), CmpValue::Int(value)) => Some(cell.cmp(value)),
                (Some(Value::Int(cell)), CmpValue::Float(value)) => {
                    super::value::int_type_to_float_type(cell).partial_cmp(value)
                }
                (Some(Value::Float(cell)), CmpValue::Int(value)) => {
                    cell.partial_cmp(&super::value::int_type_to_float_type(value))
                }
                (Some(Value::Float(cell)), CmpValue::Float(value)) => cell.partial_cmp(value),
                _ => None,
            };
            // `None` also covers NaN, which matches no comparison.
            let Some(ordering) = ordering else {
                continue;
            };
            let matches = match op {
                ">" => ordering.is_gt(),
                "<" => ordering.is_lt(),
                ">=" => ordering.is_ge(),
                "<=" => ordering.is_le(),
                "=" | "==" => ordering.is_eq(),
                "!=" => ordering.is_ne(),
                _ => false,
            };
            if matches {
                new_table.rows.push(record.clone());
            }
        }
        new_table
    }

    /// Sorts the table by the specified column in ascending order.
    pub fn order_by(&self, column: &str, ascending: bool) -> Self {
        let mut new_table = Self::new(self.columns.clone());
        new_table.rows = self.rows.clone();

        new_table.rows.sort_by(|a, b| {
            let a_val = a.get(column);
            let b_val = b.get(column);

            // Int/Float mix (a `%CPU` column holding both `3` and `3.5`, say)
            // used to fall through the catch-all as `Equal`, so two rows
            // never swapped even when one was clearly bigger. Route it
            // through the same cross-type comparison `where_cmp` uses.
            let cmp = match (a_val, b_val) {
                (Some(Value::Int(a)), Some(Value::Int(b))) => a.cmp(b),
                (Some(Value::Float(a)), Some(Value::Float(b))) => {
                    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                }
                (Some(Value::Int(a)), Some(Value::Float(b))) => {
                    super::value::int_type_to_float_type(a)
                        .partial_cmp(b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }
                (Some(Value::Float(a)), Some(Value::Int(b))) => a
                    .partial_cmp(&super::value::int_type_to_float_type(b))
                    .unwrap_or(std::cmp::Ordering::Equal),
                (Some(Value::String(a)), Some(Value::String(b))) => a.cmp(b),
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Equal,
            };

            if ascending { cmp } else { cmp.reverse() }
        });

        new_table
    }

    /// Resolves a user-supplied column name against the schema: exact match
    /// first, then case-insensitive. `None` if neither matches.
    pub fn resolve_column(&self, name: &str) -> Option<&str> {
        if let Some(exact) = self.columns.iter().find(|c| c.as_str() == name) {
            return Some(exact.as_str());
        }
        self.columns
            .iter()
            .find(|c| c.eq_ignore_ascii_case(name))
            .map(String::as_str)
    }

    /// Filters rows where the specified column does not equal the given
    /// value. The complement of `where_eq`: a missing column also counts as
    /// "not equal", matching `!=`'s everyday meaning.
    pub fn where_ne(&self, column: &str, value: &Value) -> Self {
        let mut new_table = Self::new(self.columns.clone());
        for record in &self.rows {
            if record.get(column) != Some(value) {
                new_table.rows.push(record.clone());
            }
        }
        new_table
    }

    /// Keeps only the first row for each distinct value of `column`, in
    /// original order. Rows missing the column are dropped.
    pub fn distinct(&self, column: &str) -> Self {
        let mut seen: Vec<&Value> = Vec::new();
        let mut new_table = Self::new(self.columns.clone());
        for record in &self.rows {
            let Some(value) = record.get(column) else {
                continue;
            };
            if seen.contains(&value) {
                continue;
            }
            seen.push(value);
            new_table.rows.push(record.clone());
        }
        new_table
    }

    /// Renames a column, in the schema and in every row. A row with no
    /// `old` field is left untouched.
    /// Errors if `new` collides with a *different* existing column: an
    /// unchecked rename onto an existing name would fold both columns'
    /// values under one key in each row's `IndexMap`, discarding whichever
    /// value was set second -- silently, since `Record::set` has no way to
    /// report that its key already held something.
    pub fn rename(&self, old: &str, new: &str) -> Result<Self, String> {
        if new != old && self.columns.iter().any(|c| c == new) {
            return Err(format!(
                "cannot rename '{old}' to '{new}': a column named '{new}' already exists"
            ));
        }
        let columns: Vec<String> = self
            .columns
            .iter()
            .map(|c| if c == old { new.to_string() } else { c.clone() })
            .collect();
        let mut new_table = Self::new(columns);
        for record in &self.rows {
            let mut new_record = Record::new();
            for (key, value) in &record.fields {
                let key = if key == old {
                    new.to_string()
                } else {
                    key.clone()
                };
                new_record.set(key, value.clone());
            }
            new_table.rows.push(new_record);
        }
        Ok(new_table)
    }

    /// Groups rows by the distinct values of `column`, returning a table
    /// with columns `[column, <count column>]` -- one row per distinct
    /// value, in first-seen order. The count column is named `"count"`
    /// unless `column` is itself already called that, in which case a
    /// unique variant (`"count_"`, `"count__"`, ...) is used instead --
    /// otherwise grouping by a column literally named `count` would give
    /// two columns the same name, and each row's `IndexMap` would silently
    /// keep only the second `set` (the computed count, discarding the
    /// original grouped value).
    pub fn group_by(&self, column: &str) -> Self {
        let mut order: Vec<Value> = Vec::new();
        let mut counts: Vec<usize> = Vec::new();
        for record in &self.rows {
            let Some(value) = record.get(column) else {
                continue;
            };
            match order.iter().position(|v| v == value) {
                Some(pos) => counts[pos] += 1,
                None => {
                    order.push(value.clone());
                    counts.push(1);
                }
            }
        }

        let mut count_column = "count".to_string();
        while count_column == column {
            count_column.push('_');
        }

        let mut new_table = Self::new(vec![column.to_string(), count_column.clone()]);
        for (value, count) in order.into_iter().zip(counts) {
            let mut record = Record::new();
            record.set(column.to_string(), value);
            record.set(count_column.clone(), usize_to_int_value(count));
            new_table.rows.push(record);
        }
        new_table
    }

    /// `group_by` ordered by count, descending -- "what shows up most".
    pub fn count_by(&self, column: &str) -> Self {
        self.group_by(column).order_by("count", false)
    }

    /// Sums every numeric cell in `column` using `Value`'s own numeric `+`,
    /// so the same `IntType`/`FloatType` promotion the rest of the language
    /// uses applies here too (an all-integer column stays an integer sum).
    /// Non-numeric cells are skipped; `None` if there were no numeric cells.
    pub fn sum(&self, column: &str) -> Option<Value> {
        let mut total: Option<Value> = None;
        for record in &self.rows {
            let Some(cell) = record.get(column) else {
                continue;
            };
            if !matches!(cell, Value::Int(_) | Value::Float(_)) {
                continue;
            }
            total = Some(match total {
                None => cell.clone(),
                Some(acc) => (&acc + cell).unwrap_or(acc),
            });
        }
        total
    }

    /// Averages every numeric cell in `column`. `None` if there are none.
    pub fn avg(&self, column: &str) -> Option<f64> {
        let values: Vec<f64> = self
            .rows
            .iter()
            .filter_map(|r| r.get(column))
            .filter_map(numeric_as_f64)
            .collect();
        if values.is_empty() {
            return None;
        }
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }

    /// The row cell with the smallest numeric value in `column`, in its
    /// original type (`min pid` stays an integer, not `42.0`).
    pub fn min(&self, column: &str) -> Option<Value> {
        self.numeric_extreme(column, std::cmp::Ordering::Less)
    }

    /// The row cell with the largest numeric value in `column`.
    pub fn max(&self, column: &str) -> Option<Value> {
        self.numeric_extreme(column, std::cmp::Ordering::Greater)
    }

    fn numeric_extreme(&self, column: &str, want: std::cmp::Ordering) -> Option<Value> {
        let mut best: Option<(&Value, f64)> = None;
        for record in &self.rows {
            let Some(cell) = record.get(column) else {
                continue;
            };
            let Some(n) = numeric_as_f64(cell) else {
                continue;
            };
            best = match best {
                None => Some((cell, n)),
                Some((_, best_n)) if n.partial_cmp(&best_n) == Some(want) => Some((cell, n)),
                some => some,
            };
        }
        best.map(|(v, _)| v.clone())
    }
}

/// A numeric cell as `f64`, for `avg`/`min`/`max` -- these don't need the
/// exact-integer precision `where_cmp`/`order_by` preserve via `CmpValue`.
fn numeric_as_f64(value: &Value) -> Option<f64> {
    match CmpValue::from_value(value)? {
        CmpValue::Int(n) => Some(super::value::int_type_to_float_type(&n)),
        CmpValue::Float(f) => Some(f),
    }
}

fn usize_to_int_value(n: usize) -> Value {
    cfg_if! {
        if #[cfg(feature = "bigint")] {
            Value::Int(IntType::from(n))
        } else {
            Value::Int(n as IntType)
        }
    }
}

impl Default for Table {
    fn default() -> Self {
        Self::empty()
    }
}

impl Display for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_display())
    }
}

impl PartialEq for Table {
    fn eq(&self, other: &Self) -> bool {
        self.columns == other.columns && self.rows == other.rows
    }
}

// Keys inserted into the resulting map are always `Value::String` (JSON object
// keys), so the interior mutability of other `Value` variants is irrelevant.
#[allow(clippy::mutable_key_type)]
fn json_to_value(json: &JsonValue) -> Value {
    match json {
        JsonValue::Null => Value::NIL,
        JsonValue::Bool(b) => {
            if *b {
                Value::True
            } else {
                Value::False
            }
        }
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(IntType::from(i))
            } else if let Some(f) = n.as_f64() {
                Value::Float(f as super::FloatType)
            } else {
                Value::String(n.to_string())
            }
        }
        JsonValue::String(s) => Value::String(s.clone()),
        JsonValue::Array(arr) => {
            // Convert to Lisp list
            let values: Vec<Value> = arr.iter().map(json_to_value).collect();
            Value::List(values.into_iter().collect())
        }
        JsonValue::Object(obj) => {
            // Convert to HashMap (not Table, for consistency)
            use std::collections::HashMap;
            let mut map: HashMap<Value, Value> = HashMap::new();
            for (k, v) in obj {
                map.insert(Value::String(k.clone()), json_to_value(v));
            }
            Value::HashMap(Rc::new(RefCell::new(map)))
        }
    }
}

fn value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::List(list) if list == &super::List::NIL => JsonValue::Null,
        Value::True => JsonValue::Bool(true),
        Value::False => JsonValue::Bool(false),
        Value::Int(i) => {
            cfg_if! {
                if #[cfg(feature = "bigint")] {
                    use num_traits::ToPrimitive;
                    if let Some(i64_val) = i.to_i64() {
                        JsonValue::Number(i64_val.into())
                    } else if let Some(f64_val) = i.to_f64() {
                        serde_json::Number::from_f64(f64_val)
                            .map(JsonValue::Number)
                            .unwrap_or(JsonValue::Null)
                    } else {
                        JsonValue::String(i.to_string())
                    }
                } else {
                    JsonValue::Number((*i).into())
                }
            }
        }
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        Value::String(s) => JsonValue::String(s.clone()),
        Value::Symbol(s) => JsonValue::String(s.0.clone()),
        Value::List(list) => {
            // Convert list to JSON array using IntoIterator
            let arr: Vec<JsonValue> = list.into_iter().map(|v| value_to_json(&v)).collect();
            JsonValue::Array(arr)
        }
        Value::HashMap(map) => {
            let obj: serde_json::Map<String, JsonValue> = map
                .borrow()
                .iter()
                .map(|(k, v)| (format!("{k}"), value_to_json(v)))
                .collect();
            JsonValue::Object(obj)
        }
        Value::Table(table) => {
            let t = table.borrow();
            let arr: Vec<JsonValue> = t
                .rows
                .iter()
                .map(|r| {
                    let obj: serde_json::Map<String, JsonValue> = r
                        .fields
                        .iter()
                        .map(|(k, v)| (k.clone(), value_to_json(v)))
                        .collect();
                    JsonValue::Object(obj)
                })
                .collect();
            JsonValue::Array(arr)
        }
        _ => JsonValue::String(format!("{value}")),
    }
}

#[cfg(test)]
mod tests;
