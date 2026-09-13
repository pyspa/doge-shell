//! Moving a `Table` between its in-memory form and the text formats the shell
//! exchanges with: JSON (both directions, including the single-object and
//! array-of-primitives shapes), CSV, and the tabled-rendered display string.
use super::*;

impl Table {
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
                    let val = if let Ok(n) = field.parse::<crate::lisp::model::IntType>() {
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
                Value::Float(f as crate::lisp::model::FloatType)
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

pub(super) fn value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::List(list) if list == &crate::lisp::model::List::NIL => JsonValue::Null,
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
