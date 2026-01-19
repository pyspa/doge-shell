//! Structured table data type for structured data pipelines.
//!
//! This module provides `Table` and `Record` types for handling structured data
//! like JSON objects and arrays in a tabular format.

use indexmap::IndexMap;
use serde_json::{self, Value as JsonValue};
use std::cell::RefCell;
use std::fmt::{self, Display};
use std::rc::Rc;

use super::Value;

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
    pub fn where_cmp(&self, column: &str, op: &str, value: super::IntType) -> Self {
        let mut new_table = Self::new(self.columns.clone());
        for record in &self.rows {
            if let Some(Value::Int(n)) = record.get(column) {
                let matches = match op {
                    ">" => *n > value,
                    "<" => *n < value,
                    ">=" => *n >= value,
                    "<=" => *n <= value,
                    "=" | "==" => *n == value,
                    "!=" => *n != value,
                    _ => false,
                };
                if matches {
                    new_table.rows.push(record.clone());
                }
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

            let cmp = match (a_val, b_val) {
                (Some(Value::Int(a)), Some(Value::Int(b))) => a.cmp(b),
                (Some(Value::Float(a)), Some(Value::Float(b))) => {
                    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                }
                (Some(Value::String(a)), Some(Value::String(b))) => a.cmp(b),
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Equal,
            };

            if ascending { cmp } else { cmp.reverse() }
        });

        new_table
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

/// Converts a serde_json Value to our Lisp Value.
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
                Value::Int(i as super::IntType)
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
        Value::Int(i) => JsonValue::Number((*i).into()),
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
mod tests {
    use super::*;

    #[test]
    fn test_record_basic() {
        let mut record = Record::new();
        record.set("name".to_string(), Value::String("Alice".to_string()));
        record.set("age".to_string(), Value::Int(30));

        assert_eq!(record.len(), 2);
        assert_eq!(
            record.get("name"),
            Some(&Value::String("Alice".to_string()))
        );
        assert_eq!(record.get("age"), Some(&Value::Int(30)));
        assert_eq!(record.get("missing"), None);
    }

    #[test]
    fn test_table_from_json_array() {
        let json = r#"[{"name": "Alice", "age": 30}, {"name": "Bob", "age": 25}]"#;
        let table = Table::from_json(json).unwrap();

        // Columns may be in any order due to JSON object key ordering
        let mut cols = table.columns.clone();
        cols.sort();
        assert_eq!(cols, vec!["age", "name"]);
        assert_eq!(table.len(), 2);

        assert_eq!(
            table.rows[0].get("name"),
            Some(&Value::String("Alice".to_string()))
        );
        assert_eq!(table.rows[0].get("age"), Some(&Value::Int(30)));
        assert_eq!(
            table.rows[1].get("name"),
            Some(&Value::String("Bob".to_string()))
        );
        assert_eq!(table.rows[1].get("age"), Some(&Value::Int(25)));
    }

    #[test]
    fn test_table_from_json_single_object() {
        let json = r#"{"name": "Alice", "active": true}"#;
        let table = Table::from_json(json).unwrap();

        assert_eq!(table.len(), 1);
        assert_eq!(
            table.rows[0].get("name"),
            Some(&Value::String("Alice".to_string()))
        );
        assert_eq!(table.rows[0].get("active"), Some(&Value::True));
    }

    #[test]
    fn test_table_from_json_primitives() {
        let json = r#"[1, 2, 3, 4, 5]"#;
        let table = Table::from_json(json).unwrap();

        assert_eq!(table.columns, vec!["value"]);
        assert_eq!(table.len(), 5);
        assert_eq!(table.rows[0].get("value"), Some(&Value::Int(1)));
    }

    #[test]
    fn test_table_select() {
        let json = r#"[{"a": 1, "b": 2, "c": 3}, {"a": 4, "b": 5, "c": 6}]"#;
        let table = Table::from_json(json).unwrap();

        let selected = table.select(&["a", "c"]);
        assert_eq!(selected.columns, vec!["a", "c"]);
        assert_eq!(selected.rows[0].get("a"), Some(&Value::Int(1)));
        assert_eq!(selected.rows[0].get("c"), Some(&Value::Int(3)));
        assert_eq!(selected.rows[0].get("b"), None);
    }

    #[test]
    fn test_table_head_tail() {
        let json = r#"[{"n": 1}, {"n": 2}, {"n": 3}, {"n": 4}, {"n": 5}]"#;
        let table = Table::from_json(json).unwrap();

        let head = table.head(2);
        assert_eq!(head.len(), 2);
        assert_eq!(head.rows[0].get("n"), Some(&Value::Int(1)));
        assert_eq!(head.rows[1].get("n"), Some(&Value::Int(2)));

        let tail = table.tail(2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail.rows[0].get("n"), Some(&Value::Int(4)));
        assert_eq!(tail.rows[1].get("n"), Some(&Value::Int(5)));
    }

    #[test]
    fn test_table_to_json() {
        let mut table = Table::new(vec!["name".to_string(), "age".to_string()]);
        let mut record = Record::new();
        record.set("name".to_string(), Value::String("Test".to_string()));
        record.set("age".to_string(), Value::Int(42));
        table.push(record);

        let json = table.to_json();
        assert!(json.contains("\"name\":\"Test\""));
        assert!(json.contains("\"age\":42"));
    }

    #[test]
    fn test_table_display() {
        let json = r#"[{"name": "Alice", "age": 30}]"#;
        let table = Table::from_json(json).unwrap();
        let display = table.to_display();

        assert!(display.contains("name"));
        assert!(display.contains("age"));
        assert!(display.contains("Alice"));
        assert!(display.contains("30"));
    }

    #[test]
    fn test_table_count() {
        let json = r#"[{"n": 1}, {"n": 2}, {"n": 3}]"#;
        let table = Table::from_json(json).unwrap();
        assert_eq!(table.count(), 3);

        let empty = Table::empty();
        assert_eq!(empty.count(), 0);
    }

    #[test]
    fn test_table_to_ai_context() {
        let json = r#"[{"name": "Alice", "age": 30}, {"name": "Bob", "age": 25}]"#;
        let table = Table::from_json(json).unwrap();

        let context = table.to_ai_context(1);
        assert!(context.contains("Table Schema:"));
        assert!(context.contains("Total Rows: 2"));
        assert!(context.contains("Sample Data (1 rows):"));
        assert!(context.contains("Alice"));
        assert!(context.contains("... and 1 more rows"));

        // Test with more samples than rows
        let context_all = table.to_ai_context(10);
        assert!(context_all.contains("Sample Data (2 rows):"));
        assert!(!context_all.contains("... and"));
    }

    #[test]
    fn test_table_to_ai_context_empty() {
        let table = Table::empty();
        let context = table.to_ai_context(5);
        assert!(context.contains("(no data)"));
    }

    #[test]
    fn test_table_where_eq() {
        let json = r#"[{"name": "Alice", "role": "admin"}, {"name": "Bob", "role": "user"}, {"name": "Charlie", "role": "user"}]"#;
        let table = Table::from_json(json).unwrap();

        let admins = table.where_eq("role", &Value::String("admin".to_string()));
        assert_eq!(admins.len(), 1);
        assert_eq!(
            admins.rows[0].get("name"),
            Some(&Value::String("Alice".to_string()))
        );

        let users = table.where_eq("role", &Value::String("user".to_string()));
        assert_eq!(users.len(), 2);
    }

    #[test]
    fn test_table_where_contains() {
        let json = r#"[{"msg": "hello world"}, {"msg": "goodbye world"}, {"msg": "hello space"}]"#;
        let table = Table::from_json(json).unwrap();

        let hellos = table.where_contains("msg", "hello");
        assert_eq!(hellos.len(), 2);

        let space = table.where_contains("msg", "space");
        assert_eq!(space.len(), 1);
        assert_eq!(
            space.rows[0].get("msg"),
            Some(&Value::String("hello space".to_string()))
        );
    }

    #[test]
    fn test_table_where_cmp() {
        let json = r#"[{"val": 10}, {"val": 20}, {"val": 30}, {"val": 5}]"#;
        let table = Table::from_json(json).unwrap();

        let gt_15 = table.where_cmp("val", ">", 15);
        assert_eq!(gt_15.len(), 2);

        let le_10 = table.where_cmp("val", "<=", 10);
        assert_eq!(le_10.len(), 2);
    }

    #[test]
    fn test_table_order_by() {
        let json = r#"[{"n": 3}, {"n": 1}, {"n": 2}]"#;
        let table = Table::from_json(json).unwrap();

        let asc = table.order_by("n", true);
        assert_eq!(asc.rows[0].get("n"), Some(&Value::Int(1)));
        assert_eq!(asc.rows[1].get("n"), Some(&Value::Int(2)));
        assert_eq!(asc.rows[2].get("n"), Some(&Value::Int(3)));

        let desc = table.order_by("n", false);
        assert_eq!(desc.rows[0].get("n"), Some(&Value::Int(3)));
        assert_eq!(desc.rows[1].get("n"), Some(&Value::Int(2)));
        assert_eq!(desc.rows[2].get("n"), Some(&Value::Int(1)));
    }

    #[test]
    fn test_table_csv_roundtrip() {
        let csv = "name,age,active\nAlice,30,true\nBob,25.5,false\n";
        let table = Table::from_csv(csv).expect("Failed to parse CSV");

        assert_eq!(table.len(), 2);
        assert_eq!(table.columns, vec!["name", "age", "active"]);

        // Check types (numbers logic in from_csv)
        assert_eq!(
            table.rows[0].get("name"),
            Some(&Value::String("Alice".to_string()))
        );
        assert_eq!(table.rows[0].get("age"), Some(&Value::Int(30)));
        // active is string "true" because no boolean inferred
        assert_eq!(
            table.rows[0].get("active"),
            Some(&Value::String("true".to_string()))
        );

        assert_eq!(table.rows[1].get("age"), Some(&Value::Float(25.5)));

        let output_csv = table.to_csv().expect("Failed to generate CSV");
        assert!(output_csv.contains("name,age,active"));
        assert!(output_csv.contains("Alice,30,true"));
        assert!(output_csv.contains("Bob,25.5,false"));
    }
}
