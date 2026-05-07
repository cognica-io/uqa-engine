//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of `uqa/tests/test_subquery.py`.

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn query(engine: &Engine, sql: &str) -> uqa_sql::SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn setup() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE departments (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
    );
    exec(
        &engine,
        "INSERT INTO departments (id, name) VALUES \
         (1, 'Engineering'), (2, 'Marketing'), (3, 'Sales')",
    );
    exec(
        &engine,
        "CREATE TABLE employees (\
         id INTEGER PRIMARY KEY, \
         name TEXT NOT NULL, \
         dept_id INTEGER, \
         salary REAL)",
    );
    exec(
        &engine,
        "INSERT INTO employees (id, name, dept_id, salary) VALUES \
         (1, 'Alice', 1, 90000), \
         (2, 'Bob', 2, 75000), \
         (3, 'Carol', 1, 85000), \
         (4, 'Dave', 3, 70000), \
         (5, 'Eve', NULL, 95000)",
    );
    engine
}

fn names(result: &uqa_sql::SQLResult) -> Vec<String> {
    result
        .rows
        .iter()
        .filter_map(|r| match r.get("name") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn in_subquery_basic() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE dept_id IN (SELECT id FROM departments WHERE name = 'Engineering') \
         ORDER BY name",
    );
    assert_eq!(names(&result), vec!["Alice", "Carol"]);
}

#[test]
fn in_subquery_multiple_values() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE dept_id IN (SELECT id FROM departments WHERE name != 'Sales') \
         ORDER BY name",
    );
    assert_eq!(names(&result), vec!["Alice", "Bob", "Carol"]);
}

#[test]
fn in_subquery_no_match() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE dept_id IN (SELECT id FROM departments WHERE name = 'HR')",
    );
    assert!(result.rows.is_empty());
}

#[test]
fn in_subquery_null_excluded() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees WHERE dept_id IN (SELECT id FROM departments)",
    );
    let got = names(&result);
    assert!(!got.contains(&"Eve".to_string()));
    assert_eq!(got.len(), 4);
}

#[test]
fn not_in_subquery() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE dept_id NOT IN (SELECT id FROM departments WHERE name = 'Engineering') \
         ORDER BY name",
    );
    assert_eq!(names(&result), vec!["Bob", "Dave", "Eve"]);
}

#[test]
fn in_subquery_with_aggregate() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE salary IN (SELECT MAX(salary) AS max_sal FROM employees) \
         ORDER BY name",
    );
    assert_eq!(names(&result), vec!["Eve"]);
}

#[test]
fn exists_true() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE EXISTS (SELECT 1 FROM departments WHERE name = 'Engineering') \
         ORDER BY name",
    );
    assert_eq!(result.rows.len(), 5);
}

#[test]
fn exists_false() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE EXISTS (SELECT 1 FROM departments WHERE name = 'HR')",
    );
    assert!(result.rows.is_empty());
}

#[test]
fn not_exists() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE NOT EXISTS (SELECT 1 FROM departments WHERE name = 'HR') \
         ORDER BY name",
    );
    assert_eq!(result.rows.len(), 5);
}

#[test]
fn not_exists_with_results() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees WHERE NOT EXISTS (SELECT 1 FROM departments)",
    );
    assert!(result.rows.is_empty());
}

#[test]
fn scalar_subquery_in_select() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name, (SELECT COUNT(*) FROM departments) AS dept_count \
         FROM employees ORDER BY name LIMIT 1",
    );
    assert_eq!(result.rows[0]["name"], Value::Str("Alice".into()));
    assert_eq!(result.rows[0]["dept_count"], Value::Int(3));
}

#[test]
fn scalar_subquery_aggregate() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name, (SELECT MAX(salary) FROM employees) AS max_salary \
         FROM employees WHERE id = 1",
    );
    assert_eq!(result.rows[0]["max_salary"], Value::Float(95000.0));
}

#[test]
fn scalar_subquery_empty() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name, (SELECT salary FROM employees WHERE id = 999) AS other_sal \
         FROM employees WHERE id = 1",
    );
    assert_eq!(result.rows[0]["other_sal"], Value::Null);
}

#[test]
fn in_subquery_with_like() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE dept_id IN (SELECT id FROM departments WHERE name LIKE 'Eng%') \
         ORDER BY name",
    );
    assert_eq!(names(&result), vec!["Alice", "Carol"]);
}

#[test]
fn in_subquery_with_order_and_limit() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE dept_id IN (SELECT id FROM departments) \
         ORDER BY name LIMIT 2",
    );
    assert_eq!(names(&result), vec!["Alice", "Bob"]);
}

#[test]
fn in_subquery_with_and() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE dept_id IN (SELECT id FROM departments WHERE name = 'Engineering') \
         AND salary > 87000",
    );
    assert_eq!(names(&result), vec!["Alice"]);
}

#[test]
fn multiple_subquery_conditions() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE dept_id IN (SELECT id FROM departments) \
         AND salary IN (SELECT salary FROM employees WHERE salary >= 85000) \
         ORDER BY name",
    );
    assert_eq!(names(&result), vec!["Alice", "Carol"]);
}
