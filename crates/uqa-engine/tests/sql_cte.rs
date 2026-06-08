//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `test_cte`.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult, SQLScalarFunction};
use uqa_sql::SQLError;

struct CountCalls {
    calls: Arc<AtomicUsize>,
}

impl SQLScalarFunction for CountCalls {
    fn call(&self, args: &[Value]) -> Result<Value, SQLError> {
        if !args.is_empty() {
            return Err(SQLError::BadArity {
                name: "count_calls".into(),
                expected: "0 arguments".into(),
                actual: args.len(),
            });
        }
        let value = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Value::Int(value as i64))
    }
}

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
fn cte_insert_select_materializes_cte() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "WITH src AS (SELECT 1 AS id, 10 AS value)
         INSERT INTO t(id, value)
         SELECT id, value FROM src",
    );
    let result = exec(&engine, "SELECT value FROM t WHERE id = 1");
    assert_eq!(ints(&result, "value"), vec![10]);
}

#[test]
fn insert_select_maps_explicit_columns_by_position() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE events (
            id INTEGER PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
            kind TEXT NOT NULL,
            amount INTEGER NOT NULL
        )",
    );

    exec(
        &engine,
        "INSERT INTO events(kind, amount)
         SELECT 'spawn', 7",
    );

    let result = exec(&engine, "SELECT kind, amount FROM events");
    assert_eq!(result.rows[0]["kind"], uqa_core::Value::Str("spawn".into()));
    assert_eq!(result.rows[0]["amount"], uqa_core::Value::Int(7));
}

#[test]
fn cte_update_from_materializes_cte() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(&engine, "INSERT INTO t(id, value) VALUES (1, 10)");
    exec(
        &engine,
        "WITH delta AS (SELECT 1 AS id, 7 AS amount)
         UPDATE t
         SET value = value + delta.amount
         FROM delta
         WHERE t.id = delta.id",
    );
    let result = exec(&engine, "SELECT value FROM t WHERE id = 1");
    assert_eq!(ints(&result, "value"), vec![17]);
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
fn recursive_cte_applies_output_filter_to_working_branch() {
    let engine = Engine::new();
    let calls = Arc::new(AtomicUsize::new(0));
    engine
        .register_scalar_function(
            "count_calls",
            CountCalls {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();
    exec(&engine, "CREATE TABLE seeds(id INTEGER PRIMARY KEY)");
    exec(&engine, "INSERT INTO seeds(id) VALUES (1), (2), (3), (4)");

    let result = exec(
        &engine,
        "WITH RECURSIVE walk(player_id, depth, marker) AS (
            SELECT id, 0, count_calls() FROM seeds
            UNION ALL
            SELECT w.player_id, w.depth + 1, count_calls()
            FROM walk w
            WHERE w.depth < 2
         )
         SELECT COUNT(*) AS cnt FROM walk w WHERE w.player_id = 1",
    );

    assert_eq!(result.rows[0]["cnt"], Value::Int(3));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn recursive_cte_view_applies_outer_output_filter_to_working_branch() {
    let engine = Engine::new();
    let calls = Arc::new(AtomicUsize::new(0));
    engine
        .register_scalar_function(
            "count_calls",
            CountCalls {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();
    exec(&engine, "CREATE TABLE seeds(id INTEGER PRIMARY KEY)");
    exec(&engine, "INSERT INTO seeds(id) VALUES (1), (2), (3), (4)");
    exec(
        &engine,
        "CREATE VIEW walked AS
         WITH RECURSIVE walk(player_id, depth, marker) AS (
            SELECT id, 0, count_calls() FROM seeds
            UNION ALL
            SELECT w.player_id, w.depth + 1, count_calls()
            FROM walk w
            WHERE w.depth < 2
         )
         SELECT w.player_id, COUNT(*) AS cnt
         FROM walk w
         GROUP BY w.player_id",
    );

    let result = exec(&engine, "SELECT cnt FROM walked WHERE player_id = 1");

    assert_eq!(result.rows[0]["cnt"], Value::Int(3));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
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
