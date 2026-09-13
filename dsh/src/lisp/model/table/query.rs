//! The query verbs a pipeline applies to a `Table`: column projection and
//! renaming, row slicing, the `where`/`order-by` family that compares through
//! `CmpValue`, grouping and counting, and the numeric aggregates.
use super::*;

impl Table {
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
                    crate::lisp::model::value::int_type_to_float_type(cell).partial_cmp(value)
                }
                (Some(Value::Float(cell)), CmpValue::Int(value)) => {
                    cell.partial_cmp(&crate::lisp::model::value::int_type_to_float_type(value))
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
                    crate::lisp::model::value::int_type_to_float_type(a)
                        .partial_cmp(b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }
                (Some(Value::Float(a)), Some(Value::Int(b))) => a
                    .partial_cmp(&crate::lisp::model::value::int_type_to_float_type(b))
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
        CmpValue::Int(n) => Some(crate::lisp::model::value::int_type_to_float_type(&n)),
        CmpValue::Float(f) => Some(f),
    }
}

fn usize_to_int_value(n: usize) -> Value {
    Value::Int(n as IntType)
}
