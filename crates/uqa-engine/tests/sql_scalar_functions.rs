//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for scalar SQL builtins (UPPER/LOWER/SUBSTRING/COALESCE/...),
//! `CASE WHEN`, and explicit `CAST(... AS ...)` propagation through the
//! Engine SQL surface.

use uqa_core::Value;
use uqa_engine::Engine;

fn fixture() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, body TEXT, score INTEGER, opt TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO notes (body, score, opt) VALUES ('Hello World', 7, NULL)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO notes (body, score, opt) VALUES ('rust IS great', 3, 'tag')",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn upper_lower_length() {
    let eng = fixture();
    let res = eng
        .sql(
            "SELECT id, UPPER(body) AS up, LOWER(body) AS lo, LENGTH(body) AS len \
             FROM notes ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(res.rows[0]["up"], Value::Str("HELLO WORLD".into()));
    assert_eq!(res.rows[0]["lo"], Value::Str("hello world".into()));
    assert_eq!(res.rows[0]["len"], Value::Int(11));
    assert_eq!(res.rows[1]["up"], Value::Str("RUST IS GREAT".into()));
}

#[test]
fn coalesce_picks_first_non_null() {
    let eng = fixture();
    let res = eng
        .sql(
            "SELECT id, COALESCE(opt, 'fallback') AS picked FROM notes ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(res.rows[0]["picked"], Value::Str("fallback".into()));
    assert_eq!(res.rows[1]["picked"], Value::Str("tag".into()));
}

#[test]
fn nullif_returns_null_on_match() {
    let eng = fixture();
    let res = eng
        .sql("SELECT NULLIF(score, 7) AS x FROM notes ORDER BY id", &[])
        .unwrap();
    assert_eq!(res.rows[0]["x"], Value::Null);
    assert_eq!(res.rows[1]["x"], Value::Int(3));
}

#[test]
fn case_searched_form() {
    let eng = fixture();
    let res = eng
        .sql(
            "SELECT CASE WHEN score >= 5 THEN 'hi' ELSE 'lo' END AS bucket \
             FROM notes ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(res.rows[0]["bucket"], Value::Str("hi".into()));
    assert_eq!(res.rows[1]["bucket"], Value::Str("lo".into()));
}

#[test]
fn case_simple_form_compares_against_base() {
    let eng = fixture();
    let res = eng
        .sql(
            "SELECT CASE score WHEN 7 THEN 'seven' WHEN 3 THEN 'three' ELSE 'other' END AS label \
             FROM notes ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(res.rows[0]["label"], Value::Str("seven".into()));
    assert_eq!(res.rows[1]["label"], Value::Str("three".into()));
}

#[test]
fn substring_and_concat() {
    let eng = fixture();
    let res = eng
        .sql(
            "SELECT SUBSTRING(body, 1, 5) AS prefix, CONCAT(body, '!') AS shouted \
             FROM notes ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(res.rows[0]["prefix"], Value::Str("Hello".into()));
    assert_eq!(res.rows[0]["shouted"], Value::Str("Hello World!".into()));
    assert_eq!(res.rows[1]["prefix"], Value::Str("rust ".into()));
}

#[test]
fn cast_string_to_integer_in_expression() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY, raw TEXT)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (raw) VALUES ('42')", &[]).unwrap();
    let res = eng
        .sql("SELECT CAST(raw AS INTEGER) AS n FROM t", &[])
        .unwrap();
    assert_eq!(res.rows[0]["n"], Value::Int(42));
}

#[test]
fn round_with_precision() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    let res = eng
        .sql(
            "SELECT ROUND(CAST(2.71828 AS FLOAT8), 2) AS rounded FROM t",
            &[],
        )
        .unwrap();
    assert_eq!(res.rows[0]["rounded"], Value::Float(2.72));
}

#[test]
fn regexp_match_returns_capture_group() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    let res = eng
        .sql(
            "SELECT regexp_match('Order #42 placed', '#(\\d+)') AS m FROM t",
            &[],
        )
        .unwrap();
    let m = match &res.rows[0]["m"] {
        Value::List(items) if items.len() == 1 => items[0].clone(),
        other => panic!("expected list with 1 group, got {other:?}"),
    };
    assert_eq!(m, Value::Str("42".into()));
}

#[test]
fn regexp_replace_global_flag() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    let res = eng
        .sql(
            "SELECT regexp_replace('a-b-c-d', '-', '_', 'g') AS s FROM t",
            &[],
        )
        .unwrap();
    assert_eq!(res.rows[0]["s"], Value::Str("a_b_c_d".into()));
}

#[test]
fn split_part_one_indexed() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    let res = eng
        .sql("SELECT split_part('a/b/c', '/', 2) AS mid FROM t", &[])
        .unwrap();
    assert_eq!(res.rows[0]["mid"], Value::Str("b".into()));
}

#[test]
fn greatest_least_skip_nulls() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id BIGSERIAL PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1)", &[]).unwrap();
    let res = eng
        .sql(
            "SELECT GREATEST(1, 5, 3) AS g, LEAST(NULL, 2, 7) AS l FROM t",
            &[],
        )
        .unwrap();
    assert_eq!(res.rows[0]["g"], Value::Int(5));
    assert_eq!(res.rows[0]["l"], Value::Int(2));
}
