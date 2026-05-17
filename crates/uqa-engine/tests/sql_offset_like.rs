//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `test_offset_like`.

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn engine() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            category TEXT,
            price REAL
        )",
    );
    exec(
        &engine,
        "INSERT INTO items (id, name, category, price) VALUES
            (1, 'Apple', 'fruit', 1.50),
            (2, 'Banana', 'fruit', 0.75),
            (3, 'Carrot', 'vegetable', 2.00),
            (4, 'Date', 'fruit', 5.00),
            (5, 'Eggplant', 'vegetable', 3.50),
            (6, 'Fig', 'fruit', 4.00),
            (7, 'Grape', 'fruit', 2.50),
            (8, 'Habanero', 'pepper', 1.00)",
    );
    engine
}

fn ids(result: &SQLResult) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match &row["id"] {
            Value::Int(id) => *id,
            other => panic!("expected int id, got {other:?}"),
        })
        .collect()
}

fn names(result: &SQLResult) -> Vec<String> {
    result
        .rows
        .iter()
        .map(|row| match &row["name"] {
            Value::Str(name) => name.clone(),
            other => panic!("expected text name, got {other:?}"),
        })
        .collect()
}

#[test]
fn limit_offset() {
    let engine = engine();
    let r = exec(&engine, "SELECT id FROM items ORDER BY id LIMIT 3 OFFSET 2");
    assert_eq!(ids(&r), vec![3, 4, 5]);
}

#[test]
fn offset_zero() {
    let engine = engine();
    let r = exec(&engine, "SELECT id FROM items ORDER BY id LIMIT 3 OFFSET 0");
    assert_eq!(ids(&r), vec![1, 2, 3]);
}

#[test]
fn offset_past_end() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT id FROM items ORDER BY id LIMIT 5 OFFSET 100",
    );
    assert!(r.rows.is_empty());
}

#[test]
fn offset_last_rows() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT id FROM items ORDER BY id LIMIT 10 OFFSET 6",
    );
    assert_eq!(ids(&r), vec![7, 8]);
}

#[test]
fn offset_with_where() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT id FROM items WHERE category = 'fruit' ORDER BY id LIMIT 2 OFFSET 1",
    );
    assert_eq!(ids(&r), vec![2, 4]);
}

#[test]
fn offset_with_order_desc() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT id FROM items ORDER BY id DESC LIMIT 3 OFFSET 2",
    );
    assert_eq!(ids(&r), vec![6, 5, 4]);
}

#[test]
fn offset_single_row() {
    let engine = engine();
    let r = exec(&engine, "SELECT id FROM items ORDER BY id LIMIT 1 OFFSET 4");
    assert_eq!(ids(&r), vec![5]);
}

#[test]
fn offset_with_aggregation() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT category, COUNT(*) AS cnt FROM items
         GROUP BY category ORDER BY category LIMIT 2 OFFSET 1",
    );
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0]["category"], Value::Str("pepper".into()));
    assert_eq!(r.rows[1]["category"], Value::Str("vegetable".into()));
}

#[test]
fn like_prefix() {
    let engine = engine();
    let r = exec(&engine, "SELECT name FROM items WHERE name LIKE 'A%'");
    assert_eq!(names(&r), vec!["Apple"]);
}

#[test]
fn like_suffix() {
    let engine = engine();
    let r = exec(&engine, "SELECT name FROM items WHERE name LIKE '%e'");
    let mut got = names(&r);
    got.sort();
    assert_eq!(got, vec!["Apple", "Date", "Grape"]);
}

#[test]
fn like_contains() {
    let engine = engine();
    let r = exec(&engine, "SELECT name FROM items WHERE name LIKE '%an%'");
    let mut got = names(&r);
    got.sort();
    assert_eq!(got, vec!["Banana", "Eggplant", "Habanero"]);
}

#[test]
fn like_single_char_wildcard() {
    let engine = engine();
    let r = exec(&engine, "SELECT name FROM items WHERE name LIKE '_ig'");
    assert_eq!(names(&r), vec!["Fig"]);
}

#[test]
fn like_exact_match() {
    let engine = engine();
    let r = exec(&engine, "SELECT name FROM items WHERE name LIKE 'Apple'");
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0]["name"], Value::Str("Apple".into()));
}

#[test]
fn like_no_match() {
    let engine = engine();
    let r = exec(&engine, "SELECT name FROM items WHERE name LIKE 'Xyz%'");
    assert!(r.rows.is_empty());
}

#[test]
fn like_case_sensitive() {
    let engine = engine();
    let r = exec(&engine, "SELECT name FROM items WHERE name LIKE 'apple'");
    assert!(r.rows.is_empty());
}

#[test]
fn not_like() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT name FROM items WHERE name NOT LIKE '%a%' ORDER BY name",
    );
    assert_eq!(names(&r), vec!["Apple", "Fig"]);
}

#[test]
fn ilike_case_insensitive() {
    let engine = engine();
    let r = exec(&engine, "SELECT name FROM items WHERE name ILIKE 'apple'");
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0]["name"], Value::Str("Apple".into()));
}

#[test]
fn ilike_prefix() {
    let engine = engine();
    let r = exec(&engine, "SELECT name FROM items WHERE name ILIKE 'a%'");
    assert_eq!(names(&r), vec!["Apple"]);
}

#[test]
fn ilike_suffix() {
    let engine = engine();
    let r = exec(&engine, "SELECT name FROM items WHERE name ILIKE '%E'");
    let mut got = names(&r);
    got.sort();
    assert_eq!(got, vec!["Apple", "Date", "Grape"]);
}

#[test]
fn ilike_pattern_mixed_case() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT name FROM items WHERE name ILIKE '%BANANA%'",
    );
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0]["name"], Value::Str("Banana".into()));
}

#[test]
fn not_ilike() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT name FROM items WHERE name NOT ILIKE '%A%' ORDER BY name",
    );
    assert_eq!(names(&r), vec!["Fig"]);
}

#[test]
fn like_in_case() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT name,
            CASE WHEN name LIKE 'A%' THEN 'starts_A'
                 WHEN name LIKE 'B%' THEN 'starts_B'
                 ELSE 'other' END AS grp
         FROM items ORDER BY id LIMIT 3",
    );
    assert_eq!(r.rows[0]["grp"], Value::Str("starts_A".into()));
    assert_eq!(r.rows[1]["grp"], Value::Str("starts_B".into()));
    assert_eq!(r.rows[2]["grp"], Value::Str("other".into()));
}

#[test]
fn like_with_and() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT name FROM items
         WHERE name LIKE '%a%' AND category = 'fruit'
         ORDER BY name",
    );
    assert_eq!(names(&r), vec!["Banana", "Date", "Grape"]);
}

#[test]
fn like_with_or() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT name FROM items
         WHERE name LIKE 'A%' OR name LIKE 'B%'
         ORDER BY name",
    );
    assert_eq!(names(&r), vec!["Apple", "Banana"]);
}

#[test]
fn like_with_order_and_limit() {
    let engine = engine();
    let r = exec(
        &engine,
        "SELECT name FROM items WHERE name LIKE '%a%' ORDER BY name LIMIT 2",
    );
    assert_eq!(names(&r), vec!["Banana", "Carrot"]);
}

#[test]
fn ilike_in_where_expr() {
    let engine = engine();
    let r = exec(&engine, "SELECT name FROM items WHERE name ILIKE '%egg%'");
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0]["name"], Value::Str("Eggplant".into()));
}

#[test]
fn update_where_like() {
    let engine = engine();
    exec(
        &engine,
        "UPDATE items SET category = 'tropical' WHERE name LIKE '%an%'",
    );
    let r = exec(
        &engine,
        "SELECT name FROM items WHERE category = 'tropical' ORDER BY name",
    );
    assert_eq!(names(&r), vec!["Banana", "Eggplant", "Habanero"]);
}

#[test]
fn delete_where_like() {
    let engine = engine();
    exec(&engine, "DELETE FROM items WHERE name LIKE 'E%'");
    let r = exec(&engine, "SELECT id FROM items ORDER BY id");
    let got = ids(&r);
    assert!(!got.contains(&5));
    assert_eq!(got.len(), 7);
}
