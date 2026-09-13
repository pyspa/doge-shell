//! Structured table data type for structured data pipelines.
//!
//! This module provides `Table` and `Record` types for handling structured data
//! like JSON objects and arrays in a tabular format.

use indexmap::IndexMap;
use serde_json::{self, Value as JsonValue};
use std::cell::RefCell;
use std::fmt::{self, Display};
use std::rc::Rc;

use super::{IntType, Value};

mod convert;
mod query;
use convert::value_to_json;
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

#[cfg(test)]
mod tests;
