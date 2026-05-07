//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! 1:1 port of `uqa/tests/test_types.py`.

use uqa_core::Value;
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
