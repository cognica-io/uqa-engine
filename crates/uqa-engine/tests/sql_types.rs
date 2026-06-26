//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `test_types`.

use uqa_core::{DecimalValue, Value};
use uqa_engine::{Engine, SQLResult};

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn engine_with_table() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER, name TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (1, 10, 'alpha')",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (2, 20, 'bravo')",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, name) VALUES (3, 30, 'charlie')",
    );
    engine
}

fn dec(value: &str) -> Value {
    Value::Decimal(DecimalValue::parse(value).unwrap())
}

#[test]
fn numeric_literal_arithmetic_is_decimal() {
    let engine = Engine::new();
    let result = exec(&engine, "SELECT 0.1 + 0.2 AS v");
    assert_eq!(result.rows[0]["v"], dec("0.3"));
}

#[test]
fn numeric_column_rounds_to_declared_scale() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE prices (id INTEGER PRIMARY KEY, amount NUMERIC(10, 2))",
    );
    exec(
        &engine,
        "INSERT INTO prices (id, amount) VALUES (1, 12.345)",
    );
    let result = exec(&engine, "SELECT amount FROM prices WHERE id = 1");
    assert_eq!(result.rows[0]["amount"], dec("12.35"));
}

#[test]
fn numeric_negative_scale_rounds_left_of_decimal() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE buckets (id INTEGER PRIMARY KEY, amount NUMERIC(2, -3))",
    );
    exec(
        &engine,
        "INSERT INTO buckets (id, amount) VALUES (1, 12345)",
    );
    let result = exec(&engine, "SELECT amount FROM buckets WHERE id = 1");
    assert_eq!(result.rows[0]["amount"], dec("12000"));
}

#[test]
fn numeric_negative_scale_enforces_precision_after_rounding() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE buckets (id INTEGER PRIMARY KEY, amount NUMERIC(2, -3))",
    );
    let err = engine
        .sql("INSERT INTO buckets (id, amount) VALUES (1, 99999)", &[])
        .unwrap_err();
    assert!(err.to_string().contains("numeric field overflow"));
}

#[test]
fn numeric_scale_larger_than_precision_restricts_fractional_range() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE tiny (id INTEGER PRIMARY KEY, amount NUMERIC(3, 5))",
    );
    exec(
        &engine,
        "INSERT INTO tiny (id, amount) VALUES (1, 0.009994)",
    );
    let result = exec(&engine, "SELECT amount FROM tiny WHERE id = 1");
    assert_eq!(result.rows[0]["amount"], dec("0.00999"));

    let err = engine
        .sql("INSERT INTO tiny (id, amount) VALUES (2, 0.009995)", &[])
        .unwrap_err();
    assert!(err.to_string().contains("numeric field overflow"));
}

#[test]
fn numeric_information_schema_reports_negative_scale() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE buckets (id INTEGER PRIMARY KEY, amount NUMERIC(2, -3))",
    );
    let result = exec(
        &engine,
        "SELECT numeric_precision, numeric_scale
         FROM information_schema.columns
         WHERE table_name = 'buckets' AND column_name = 'amount'",
    );
    assert_eq!(result.rows[0]["numeric_precision"], Value::Int(2));
    assert_eq!(result.rows[0]["numeric_scale"], Value::Int(-3));
}

#[test]
fn numeric_cast_preserves_decimal_value() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "SELECT CAST('123456789012345.6789' AS NUMERIC) AS v",
    );
    assert_eq!(result.rows[0]["v"], dec("123456789012345.6789"));
}

#[test]
fn select_array_literal() {
    let engine = engine_with_table();
    let result = exec(&engine, "SELECT ARRAY[1, 2, 3] AS v FROM t WHERE id = 1");
    assert_eq!(
        result.rows[0]["v"],
        Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
}

#[test]
fn select_text_array_literal() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT ARRAY['a', 'b', 'c'] AS v FROM t WHERE id = 1",
    );
    assert_eq!(
        result.rows[0]["v"],
        Value::List(vec![
            Value::Str("a".into()),
            Value::Str("b".into()),
            Value::Str("c".into()),
        ])
    );
}

#[test]
fn select_empty_array_literal() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT ARRAY[]::integer[] AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::List(Vec::new()));
}

#[test]
fn text_array_column_create_insert_round_trip() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE arr_test (id SERIAL PRIMARY KEY, tags TEXT[])",
    );
    exec(
        &engine,
        "INSERT INTO arr_test (tags) VALUES (ARRAY['python', 'sql'])",
    );
    let result = exec(&engine, "SELECT tags FROM arr_test WHERE id = 1");
    assert_eq!(
        result.rows[0]["tags"],
        Value::List(vec![Value::Str("python".into()), Value::Str("sql".into())])
    );
}

#[test]
fn integer_array_column_create_insert_round_trip() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE int_arr (id SERIAL PRIMARY KEY, nums INTEGER[])",
    );
    exec(
        &engine,
        "INSERT INTO int_arr (nums) VALUES (ARRAY[10, 20, 30])",
    );
    let result = exec(&engine, "SELECT nums FROM int_arr WHERE id = 1");
    assert_eq!(
        result.rows[0]["nums"],
        Value::List(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
    );
}

#[test]
fn array_length_returns_length() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT array_length(ARRAY[1, 2, 3], 1) AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Int(3));
}

#[test]
fn cardinality_returns_array_length() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT cardinality(ARRAY[1, 2, 3]) AS v FROM t WHERE id = 1",
    );
    assert_eq!(result.rows[0]["v"], Value::Int(3));
}

#[test]
fn array_cat_concatenates() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT array_cat(ARRAY[1, 2], ARRAY[3, 4]) AS v FROM t WHERE id = 1",
    );
    assert_eq!(
        result.rows[0]["v"],
        Value::List(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4)
        ])
    );
}

#[test]
fn array_append_appends() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT array_append(ARRAY[1, 2], 3) AS v FROM t WHERE id = 1",
    );
    assert_eq!(
        result.rows[0]["v"],
        Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
}

#[test]
fn array_remove_removes_matching_values() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT array_remove(ARRAY[1, 2, 3, 2], 2) AS v FROM t WHERE id = 1",
    );
    assert_eq!(
        result.rows[0]["v"],
        Value::List(vec![Value::Int(1), Value::Int(3)])
    );
}

#[test]
fn gen_random_uuid_returns_uuid_shaped_string() {
    let engine = engine_with_table();
    let result = exec(&engine, "SELECT gen_random_uuid() AS v FROM t WHERE id = 1");
    let Value::Str(v) = &result.rows[0]["v"] else {
        panic!("expected uuid string, got {:?}", result.rows[0]["v"]);
    };
    let parts: Vec<_> = v.split('-').collect();
    assert_eq!(
        parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
        vec![8, 4, 4, 4, 12]
    );
}

#[test]
fn uuid_column_round_trips_as_text() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE uuid_test (id SERIAL PRIMARY KEY, uid UUID)",
    );
    exec(
        &engine,
        "INSERT INTO uuid_test (uid) VALUES ('550e8400-e29b-41d4-a716-446655440000')",
    );
    let result = exec(&engine, "SELECT uid FROM uuid_test WHERE id = 1");
    assert_eq!(
        result.rows[0]["uid"],
        Value::Str("550e8400-e29b-41d4-a716-446655440000".into())
    );
}

#[test]
fn gen_random_uuid_returns_unique_values() {
    let engine = engine_with_table();
    let result = exec(
        &engine,
        "SELECT gen_random_uuid() AS a, gen_random_uuid() AS b FROM t WHERE id = 1",
    );
    assert_ne!(result.rows[0]["a"], result.rows[0]["b"]);
}

#[test]
fn bytea_column_accepts_text_input() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE bin_test (id SERIAL PRIMARY KEY, data BYTEA)",
    );
    exec(&engine, "INSERT INTO bin_test (data) VALUES ('hello')");
    let result = exec(&engine, "SELECT data FROM bin_test WHERE id = 1");
    assert_ne!(result.rows[0]["data"], Value::Null);
}

#[test]
fn cast_text_to_bytea_returns_bytes() {
    let engine = engine_with_table();
    let result = exec(&engine, "SELECT 'hello'::bytea AS v FROM t WHERE id = 1");
    assert_eq!(result.rows[0]["v"], Value::Bytes(b"hello".to_vec()));
}
