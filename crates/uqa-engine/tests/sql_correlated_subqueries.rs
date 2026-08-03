//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Correlated-subquery coverage.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use uqa_core::Value;
use uqa_engine::{Engine, SQLFunctionOptions, SQLFunctionVolatility};

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
         (5, 'Eve', 1, 95000), \
         (6, 'Frank', 2, 80000)",
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
fn salary_above_dept_avg() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT e.name FROM employees e \
         WHERE e.salary > (\
           SELECT AVG(salary) FROM employees \
           WHERE dept_id = e.dept_id\
         ) ORDER BY e.name",
    );
    assert_eq!(names(&result), vec!["Eve", "Frank"]);
}

#[test]
fn salary_equal_dept_max() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT e.name FROM employees e \
         WHERE e.salary = (\
           SELECT MAX(salary) FROM employees \
           WHERE dept_id = e.dept_id\
         ) ORDER BY e.name",
    );
    assert_eq!(names(&result), vec!["Dave", "Eve", "Frank"]);
}

#[test]
fn correlated_count() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT e.name FROM employees e \
         WHERE (\
           SELECT COUNT(*) FROM employees \
           WHERE dept_id = e.dept_id\
         ) > 1 \
         ORDER BY e.name",
    );
    assert_eq!(
        names(&result),
        vec!["Alice", "Bob", "Carol", "Eve", "Frank"]
    );
}

#[test]
fn exists_basic() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT d.name FROM departments d \
         WHERE EXISTS (\
           SELECT 1 FROM employees e WHERE e.dept_id = d.id\
         ) ORDER BY d.name",
    );
    assert_eq!(names(&result), vec!["Engineering", "Marketing", "Sales"]);
}

#[test]
fn not_exists() {
    let engine = setup();
    exec(
        &engine,
        "INSERT INTO departments (id, name) VALUES (4, 'HR')",
    );
    let result = query(
        &engine,
        "SELECT d.name FROM departments d \
         WHERE NOT EXISTS (\
           SELECT 1 FROM employees e WHERE e.dept_id = d.id\
         ) ORDER BY d.name",
    );
    assert_eq!(names(&result), vec!["HR"]);
}

#[test]
fn not_exists_with_composite_key_and_residual_filter() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE candidates (
            id INTEGER PRIMARY KEY,
            x REAL,
            y REAL,
            active INTEGER
        )",
    );
    exec(
        &engine,
        "CREATE TABLE walls (
            x INTEGER,
            y INTEGER,
            tile TEXT
        )",
    );
    // Coordinates chosen so PostgreSQL 17 float -> int casts (round
    // half to even, not truncation) hit the '#' wall for candidate 1.
    exec(
        &engine,
        "INSERT INTO candidates (id, x, y, active) VALUES
            (1, 1.2, 2.4, 1),
            (2, 4.1, 5.0, 1),
            (3, 6.0, 7.0, 0)",
    );
    exec(
        &engine,
        "INSERT INTO walls (x, y, tile) VALUES
            (1, 2, '#'),
            (6, 7, '#'),
            (4, 5, '.')",
    );

    let result = query(
        &engine,
        "SELECT id FROM candidates c
         WHERE c.active = 1
           AND NOT EXISTS (
             SELECT 1 FROM walls w
             WHERE w.x = CAST(c.x AS INT)
               AND w.y = CAST(c.y AS INT)
               AND w.tile = '#'
           )
         ORDER BY id",
    );

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["id"], Value::Int(2));
}

#[test]
fn exists_with_additional_condition() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT d.name FROM departments d \
         WHERE EXISTS (\
           SELECT 1 FROM employees e \
           WHERE e.dept_id = d.id AND e.salary > 90000\
         ) ORDER BY d.name",
    );
    assert_eq!(names(&result), vec!["Engineering"]);
}

#[test]
fn correlated_in() {
    let engine = setup();
    exec(
        &engine,
        "CREATE TABLE managers (id INTEGER PRIMARY KEY, dept_id INTEGER, level INTEGER)",
    );
    exec(
        &engine,
        "INSERT INTO managers (id, dept_id, level) VALUES (1, 1, 5), (2, 2, 3)",
    );
    let result = query(
        &engine,
        "SELECT e.name FROM employees e \
         WHERE e.dept_id IN (\
           SELECT m.dept_id FROM managers m WHERE m.level > 2\
         ) ORDER BY e.name",
    );
    assert_eq!(
        names(&result),
        vec!["Alice", "Bob", "Carol", "Eve", "Frank"]
    );
}

#[test]
fn correlated_with_min() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT e.name FROM employees e \
         WHERE e.salary = (\
           SELECT MIN(salary) FROM employees \
           WHERE dept_id = e.dept_id\
         ) ORDER BY e.name",
    );
    assert_eq!(names(&result), vec!["Bob", "Carol", "Dave"]);
}

#[test]
fn correlated_subquery_in_and() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT e.name FROM employees e \
         WHERE e.dept_id = 1 AND e.salary > (\
           SELECT AVG(salary) FROM employees \
           WHERE dept_id = e.dept_id\
         ) ORDER BY e.name",
    );
    assert_eq!(names(&result), vec!["Eve"]);
}

#[test]
fn non_correlated_still_works() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE dept_id IN (\
           SELECT id FROM departments WHERE name = 'Engineering'\
         ) ORDER BY name",
    );
    assert_eq!(names(&result), vec!["Alice", "Carol", "Eve"]);
}

#[test]
fn exists_non_correlated_still_works() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT name FROM employees \
         WHERE EXISTS (SELECT 1 FROM departments WHERE id = 1) \
         ORDER BY name",
    );
    assert_eq!(result.rows.len(), 6);
}

#[test]
fn non_correlated_scalar_subquery_runs_once_per_statement() {
    let engine = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    engine
        .register_scalar_function_with_options(
            "count_subquery_calls",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |args: &[Value]| {
                assert!(args.is_empty());
                Ok(Value::Int(
                    observed.fetch_add(1, Ordering::SeqCst) as i64 + 1,
                ))
            },
        )
        .unwrap();

    let first = query(
        &engine,
        "SELECT name FROM employees
         WHERE (SELECT count_subquery_calls()) = 1",
    );
    assert_eq!(first.rows.len(), 6);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let second = query(
        &engine,
        "SELECT name FROM employees
         WHERE (SELECT count_subquery_calls()) = 2",
    );
    assert_eq!(second.rows.len(), 6);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn correlated_scalar_subquery_still_runs_for_each_outer_row() {
    let engine = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    engine
        .register_scalar_function_with_options(
            "count_correlated_calls",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |args: &[Value]| {
                assert!(args.is_empty());
                Ok(Value::Int(
                    observed.fetch_add(1, Ordering::SeqCst) as i64 + 1,
                ))
            },
        )
        .unwrap();

    let result = query(
        &engine,
        "SELECT e.name FROM employees e
         WHERE (SELECT count_correlated_calls()
                FROM departments d WHERE d.id = e.dept_id) > 0",
    );
    assert_eq!(result.rows.len(), 6);
    assert_eq!(calls.load(Ordering::SeqCst), 6);
}

#[test]
fn unqualified_outer_reference_remains_correlated() {
    let engine = setup();
    let result = query(
        &engine,
        "SELECT e.name FROM employees e
         WHERE EXISTS (
             SELECT 1 FROM departments d WHERE dept_id = d.id
         )
         ORDER BY e.name",
    );
    assert_eq!(
        names(&result),
        vec!["Alice", "Bob", "Carol", "Dave", "Eve", "Frank"]
    );
}
