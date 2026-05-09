//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! 1:1 port of `uqa/tests/test_cte.py`.

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn engine() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE departments (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
    );
    exec(
        &engine,
        "INSERT INTO departments (id, name) VALUES
            (1, 'Engineering'),
            (2, 'Marketing'),
            (3, 'Sales')",
    );
    exec(
        &engine,
        "CREATE TABLE employees (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            dept_id INTEGER,
            salary REAL
        )",
    );
    exec(
        &engine,
        "INSERT INTO employees (id, name, dept_id, salary) VALUES
            (1, 'Alice', 1, 90000),
            (2, 'Bob', 2, 75000),
            (3, 'Carol', 1, 85000),
            (4, 'Dave', 3, 70000),
            (5, 'Eve', 1, 95000)",
    );
    engine
}

fn names(result: &SQLResult, column: &str) -> Vec<String> {
    result
        .rows
        .iter()
        .map(|row| match &row[column] {
            Value::Str(s) => s.clone(),
            other => panic!("expected string, got {other:?}"),
        })
        .collect()
}

fn ints(result: &SQLResult, column: &str) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match &row[column] {
            Value::Int(n) => *n,
            other => panic!("expected int, got {other:?}"),
        })
        .collect()
}

#[test]
fn simple_cte() {
    let engine = engine();
    let r = exec(
        &engine,
        "WITH eng AS (
            SELECT name FROM employees WHERE dept_id = 1
         )
         SELECT name FROM eng ORDER BY name",
    );
    assert_eq!(names(&r, "name"), vec!["Alice", "Carol", "Eve"]);
}

#[test]
fn cte_with_filter() {
    let engine = engine();
    let r = exec(
        &engine,
        "WITH high_sal AS (
            SELECT name, salary FROM employees WHERE salary > 80000
         )
         SELECT name FROM high_sal WHERE salary > 90000",
    );
    assert_eq!(names(&r, "name"), vec!["Eve"]);
}

#[test]
fn cte_with_aggregate() {
    let engine = engine();
    let r = exec(
        &engine,
        "WITH dept_stats AS (
            SELECT dept_id, COUNT(*) AS cnt, AVG(salary) AS avg_sal
            FROM employees GROUP BY dept_id
         )
         SELECT dept_id, cnt FROM dept_stats ORDER BY dept_id",
    );
    assert_eq!(r.rows[0]["dept_id"], Value::Int(1));
    assert_eq!(r.rows[0]["cnt"], Value::Int(3));
    assert_eq!(r.rows[1]["cnt"], Value::Int(1));
    assert_eq!(r.rows[2]["cnt"], Value::Int(1));
}

#[test]
fn cte_with_order_and_limit() {
    let engine = engine();
    let r = exec(
        &engine,
        "WITH ranked AS (
            SELECT name, salary FROM employees ORDER BY salary DESC
         )
         SELECT name FROM ranked LIMIT 3",
    );
    let got = names(&r, "name");
    assert_eq!(got.len(), 3);
    assert_eq!(got[0], "Eve");
}

#[test]
fn cte_cleanup() {
    let engine = engine();
    exec(
        &engine,
        "WITH temp_cte AS (SELECT 1 AS val) SELECT val FROM temp_cte",
    );
    assert!(!engine.has_table("temp_cte"));
}

#[test]
fn cte_does_not_shadow_real_table() {
    let engine = engine();
    exec(
        &engine,
        "WITH x AS (SELECT name FROM employees LIMIT 1) SELECT name FROM x",
    );
    let r = exec(&engine, "SELECT COUNT(*) AS cnt FROM employees");
    assert_eq!(r.rows[0]["cnt"], Value::Int(5));
}

#[test]
fn two_ctes() {
    let engine = engine();
    let r = exec(
        &engine,
        "WITH
            eng AS (SELECT id, name FROM employees WHERE dept_id = 1),
            mkt AS (SELECT id, name FROM employees WHERE dept_id = 2)
         SELECT name FROM eng ORDER BY name",
    );
    assert_eq!(names(&r, "name"), vec!["Alice", "Carol", "Eve"]);
}

#[test]
fn second_cte_used() {
    let engine = engine();
    let r = exec(
        &engine,
        "WITH
            eng AS (SELECT name FROM employees WHERE dept_id = 1),
            mkt AS (SELECT name FROM employees WHERE dept_id = 2)
         SELECT name FROM mkt",
    );
    assert_eq!(names(&r, "name"), vec!["Bob"]);
}

#[test]
fn cte_referencing_another() {
    let engine = engine();
    let r = exec(
        &engine,
        "WITH
            high_sal AS (SELECT name, salary FROM employees WHERE salary > 80000),
            very_high AS (SELECT name FROM high_sal WHERE salary > 90000)
         SELECT name FROM very_high",
    );
    assert_eq!(names(&r, "name"), vec!["Eve"]);
}

#[test]
fn cte_used_in_subquery() {
    let engine = engine();
    let r = exec(
        &engine,
        "WITH eng_ids AS (
            SELECT id FROM employees WHERE dept_id = 1
         )
         SELECT name FROM employees
         WHERE id IN (SELECT id FROM eng_ids)
         ORDER BY name",
    );
    assert_eq!(names(&r, "name"), vec!["Alice", "Carol", "Eve"]);
}

#[test]
fn cte_with_distinct() {
    let engine = engine();
    let r = exec(
        &engine,
        "WITH dept_ids AS (
            SELECT DISTINCT dept_id FROM employees
         )
         SELECT dept_id FROM dept_ids ORDER BY dept_id",
    );
    assert_eq!(ints(&r, "dept_id"), vec![1, 2, 3]);
}

#[test]
fn recursive_count() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "WITH RECURSIVE cnt(x) AS (
            SELECT 1
            UNION ALL
            SELECT x + 1 FROM cnt WHERE x < 5
         ) SELECT x FROM cnt",
    );
    assert_eq!(ints(&result, "x"), vec![1, 2, 3, 4, 5]);
}

#[test]
fn recursive_union_dedup() {
    let engine = Engine::new();
    let result = exec(
        &engine,
        "WITH RECURSIVE seq(n) AS (
            SELECT 1
            UNION
            SELECT n + 1 FROM seq WHERE n < 3
         ) SELECT n FROM seq",
    );
    let mut values = ints(&result, "n");
    values.sort_unstable();
    assert_eq!(values, vec![1, 2, 3]);
}

#[test]
fn recursive_hierarchy() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE employees (
            eid INTEGER PRIMARY KEY, ename TEXT, manager_id INTEGER
         )",
    );
    exec(
        &engine,
        "INSERT INTO employees (eid, ename, manager_id) VALUES
            (1, 'CEO', 0),
            (2, 'VP', 1),
            (3, 'Manager', 2),
            (4, 'Developer', 3)",
    );
    let result = exec(
        &engine,
        "WITH RECURSIVE chain(cid, cname, lvl) AS (
            SELECT eid, ename, 0 FROM employees WHERE eid = 1
            UNION ALL
            SELECT e.eid, e.ename, c.lvl + 1
            FROM employees e
            INNER JOIN chain c ON e.manager_id = c.cid
         ) SELECT cname, lvl FROM chain",
    );
    assert_eq!(result.rows.len(), 4);
    let got: std::collections::BTreeSet<_> = names(&result, "cname").into_iter().collect();
    assert_eq!(
        got,
        ["CEO", "VP", "Manager", "Developer"]
            .into_iter()
            .map(String::from)
            .collect()
    );
}

#[test]
fn recursive_empty_base() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)");
    let result = exec(
        &engine,
        "WITH RECURSIVE r(n) AS (
            SELECT id FROM t WHERE id = 999
            UNION ALL
            SELECT n + 1 FROM r WHERE n < 5
         ) SELECT n FROM r",
    );
    assert!(result.rows.is_empty());
}
