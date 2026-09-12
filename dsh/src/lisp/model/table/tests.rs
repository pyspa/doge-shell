use super::*;

#[test]
fn test_record_basic() {
    let mut record = Record::new();
    record.set("name".to_string(), Value::String("Alice".to_string()));
    record.set("age".to_string(), Value::Int(IntType::from(30)));

    assert_eq!(record.len(), 2);
    assert_eq!(
        record.get("name"),
        Some(&Value::String("Alice".to_string()))
    );
    assert_eq!(record.get("age"), Some(&Value::Int(IntType::from(30))));
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
    assert_eq!(
        table.rows[0].get("age"),
        Some(&Value::Int(IntType::from(30)))
    );
    assert_eq!(
        table.rows[1].get("name"),
        Some(&Value::String("Bob".to_string()))
    );
    assert_eq!(
        table.rows[1].get("age"),
        Some(&Value::Int(IntType::from(25)))
    );
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
    assert_eq!(
        table.rows[0].get("value"),
        Some(&Value::Int(IntType::from(1)))
    );
}

#[test]
fn test_table_select() {
    let json = r#"[{"a": 1, "b": 2, "c": 3}, {"a": 4, "b": 5, "c": 6}]"#;
    let table = Table::from_json(json).unwrap();

    let selected = table.select(&["a", "c"]);
    assert_eq!(selected.columns, vec!["a", "c"]);
    assert_eq!(
        selected.rows[0].get("a"),
        Some(&Value::Int(IntType::from(1)))
    );
    assert_eq!(
        selected.rows[0].get("c"),
        Some(&Value::Int(IntType::from(3)))
    );
    assert_eq!(selected.rows[0].get("b"), None);
}

#[test]
fn test_table_head_tail() {
    let json = r#"[{"n": 1}, {"n": 2}, {"n": 3}, {"n": 4}, {"n": 5}]"#;
    let table = Table::from_json(json).unwrap();

    let head = table.head(2);
    assert_eq!(head.len(), 2);
    assert_eq!(head.rows[0].get("n"), Some(&Value::Int(IntType::from(1))));
    assert_eq!(head.rows[1].get("n"), Some(&Value::Int(IntType::from(2))));

    let tail = table.tail(2);
    assert_eq!(tail.len(), 2);
    assert_eq!(tail.rows[0].get("n"), Some(&Value::Int(IntType::from(4))));
    assert_eq!(tail.rows[1].get("n"), Some(&Value::Int(IntType::from(5))));
}

#[test]
fn test_table_to_json() {
    let mut table = Table::new(vec!["name".to_string(), "age".to_string()]);
    let mut record = Record::new();
    record.set("name".to_string(), Value::String("Test".to_string()));
    record.set("age".to_string(), Value::Int(IntType::from(42)));
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

    let gt_15 = table.where_cmp("val", ">", &CmpValue::Int(IntType::from(15)));
    assert_eq!(gt_15.len(), 2);

    let le_10 = table.where_cmp("val", "<=", &CmpValue::Int(IntType::from(10)));
    assert_eq!(le_10.len(), 2);
}

#[test]
fn test_table_where_cmp_matches_float_cells() {
    let json = r#"[{"cpu": 3.5}, {"cpu": 55.0}, {"cpu": 12}]"#;
    let table = Table::from_json(json).unwrap();

    let hot = table.where_cmp("cpu", ">", &CmpValue::Float(10.0));
    assert_eq!(hot.len(), 2);
    let cool = table.where_cmp("cpu", "<=", &CmpValue::Float(3.5));
    assert_eq!(cool.len(), 1);
}

#[test]
fn test_table_where_cmp_keeps_large_integers_exact() {
    // Both values round to the same f64, so comparing as floats would
    // wrongly report them equal.
    let json = r#"[{"id": 9007199254740993}]"#;
    let table = Table::from_json(json).unwrap();
    let needle = CmpValue::Int(IntType::from(9007199254740992i64));

    assert_eq!(table.where_cmp("id", "=", &needle).len(), 0);
    assert_eq!(table.where_cmp("id", "!=", &needle).len(), 1);
    assert_eq!(table.where_cmp("id", ">", &needle).len(), 1);
}

#[test]
fn test_table_order_by() {
    let json = r#"[{"n": 3}, {"n": 1}, {"n": 2}]"#;
    let table = Table::from_json(json).unwrap();

    let asc = table.order_by("n", true);
    assert_eq!(asc.rows[0].get("n"), Some(&Value::Int(IntType::from(1))));
    assert_eq!(asc.rows[1].get("n"), Some(&Value::Int(IntType::from(2))));
    assert_eq!(asc.rows[2].get("n"), Some(&Value::Int(IntType::from(3))));

    let desc = table.order_by("n", false);
    assert_eq!(desc.rows[0].get("n"), Some(&Value::Int(IntType::from(3))));
    assert_eq!(desc.rows[1].get("n"), Some(&Value::Int(IntType::from(2))));
    assert_eq!(desc.rows[2].get("n"), Some(&Value::Int(IntType::from(1))));
}

#[test]
fn test_table_order_by_mixed_int_and_float_cells() {
    // Before the fix this fell into the catch-all `Equal` arm, so mixed
    // Int/Float cells (a `%CPU` column holding both `3` and `3.5`, say)
    // never swapped no matter how different the values were.
    let json = r#"[{"n": 3}, {"n": 1.5}, {"n": 2}]"#;
    let table = Table::from_json(json).unwrap();

    let asc = table.order_by("n", true);
    assert_eq!(asc.rows[0].get("n"), Some(&Value::Float(1.5)));
    assert_eq!(asc.rows[1].get("n"), Some(&Value::Int(IntType::from(2))));
    assert_eq!(asc.rows[2].get("n"), Some(&Value::Int(IntType::from(3))));
}

#[test]
fn test_resolve_column_is_case_insensitive() {
    let json = r#"[{"cpu": 1}]"#;
    let table = Table::from_json(json).unwrap();

    assert_eq!(table.resolve_column("cpu"), Some("cpu"));
    assert_eq!(table.resolve_column("CPU"), Some("cpu"));
    assert_eq!(table.resolve_column("missing"), None);
}

#[test]
fn test_table_where_ne() {
    let json = r#"[{"status": "Up"}, {"status": "Down"}, {"status": "Up"}]"#;
    let table = Table::from_json(json).unwrap();

    let not_up = table.where_ne("status", &Value::String("Up".to_string()));
    assert_eq!(not_up.len(), 1);
    assert_eq!(
        not_up.rows[0].get("status"),
        Some(&Value::String("Down".to_string()))
    );
}

#[test]
fn test_table_distinct() {
    let json = r#"[{"user": "a"}, {"user": "b"}, {"user": "a"}]"#;
    let table = Table::from_json(json).unwrap();

    let distinct = table.distinct("user");
    assert_eq!(distinct.len(), 2);
    assert_eq!(
        distinct.rows[0].get("user"),
        Some(&Value::String("a".to_string()))
    );
    assert_eq!(
        distinct.rows[1].get("user"),
        Some(&Value::String("b".to_string()))
    );
}

#[test]
fn test_table_rename() {
    let json = r#"[{"cpu": 1}, {"cpu": 2}]"#;
    let table = Table::from_json(json).unwrap();

    let renamed = table.rename("cpu", "cpu_percent").unwrap();
    assert_eq!(renamed.columns, vec!["cpu_percent"]);
    assert_eq!(
        renamed.rows[0].get("cpu_percent"),
        Some(&Value::Int(IntType::from(1)))
    );
    assert_eq!(renamed.rows[0].get("cpu"), None);
}

#[test]
fn test_table_rename_onto_an_existing_column_errors_instead_of_merging() {
    let json = r#"[{"user": "alice", "cpu": 5}]"#;
    let table = Table::from_json(json).unwrap();

    // Without this guard, both columns would end up under one
    // `IndexMap` key and "alice" would be lost.
    assert!(table.rename("cpu", "user").is_err());

    // Renaming a column to its own name is not a collision.
    assert!(table.rename("cpu", "cpu").is_ok());
}

#[test]
fn test_table_group_by_and_count_by() {
    let json = r#"[{"user": "a"}, {"user": "b"}, {"user": "a"}, {"user": "a"}, {"user": "b"}]"#;
    let table = Table::from_json(json).unwrap();

    let grouped = table.group_by("user");
    assert_eq!(grouped.columns, vec!["user", "count"]);
    assert_eq!(grouped.len(), 2);
    assert_eq!(
        grouped.rows[0].get("user"),
        Some(&Value::String("a".to_string()))
    );
    assert_eq!(
        grouped.rows[0].get("count"),
        Some(&Value::Int(IntType::from(3)))
    );

    let counted = table.count_by("user");
    // Descending by count: "a" (3) before "b" (2).
    assert_eq!(
        counted.rows[0].get("user"),
        Some(&Value::String("a".to_string()))
    );
    assert_eq!(
        counted.rows[0].get("count"),
        Some(&Value::Int(IntType::from(3)))
    );
    assert_eq!(
        counted.rows[1].get("count"),
        Some(&Value::Int(IntType::from(2)))
    );
}

#[test]
fn test_table_group_by_a_column_already_named_count_does_not_collide() {
    // Without the collision guard, columns would be ["count", "count"]
    // and each row's `set("count", computed_count)` would silently
    // overwrite the original grouped value under the same key.
    let json = r#"[{"count": "a"}, {"count": "b"}, {"count": "a"}]"#;
    let table = Table::from_json(json).unwrap();

    let grouped = table.group_by("count");
    assert_eq!(grouped.columns, vec!["count", "count_"]);
    assert_eq!(grouped.len(), 2);
    assert_eq!(
        grouped.rows[0].get("count"),
        Some(&Value::String("a".to_string()))
    );
    assert_eq!(
        grouped.rows[0].get("count_"),
        Some(&Value::Int(IntType::from(2)))
    );
}

#[test]
fn test_table_sum_avg_min_max() {
    let json = r#"[{"cpu": 10}, {"cpu": 20.5}, {"cpu": 30}]"#;
    let table = Table::from_json(json).unwrap();

    assert_eq!(table.sum("cpu"), Some(Value::Float(60.5)));
    assert_eq!(table.avg("cpu"), Some(60.5 / 3.0));
    assert_eq!(table.min("cpu"), Some(Value::Int(IntType::from(10))));
    assert_eq!(table.max("cpu"), Some(Value::Int(IntType::from(30))));
}

#[test]
fn test_table_sum_all_integers_stays_integer() {
    let json = r#"[{"n": 10}, {"n": 20}, {"n": 30}]"#;
    let table = Table::from_json(json).unwrap();
    assert_eq!(table.sum("n"), Some(Value::Int(IntType::from(60))));
}

#[test]
fn test_table_aggregates_on_missing_or_nonnumeric_column() {
    let json = r#"[{"name": "a"}, {"name": "b"}]"#;
    let table = Table::from_json(json).unwrap();

    assert_eq!(table.sum("name"), None);
    assert_eq!(table.avg("name"), None);
    assert_eq!(table.min("name"), None);
    assert_eq!(table.max("missing"), None);
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
    assert_eq!(
        table.rows[0].get("age"),
        Some(&Value::Int(IntType::from(30)))
    );
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
