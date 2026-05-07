//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! 1:1 port of `uqa/tests/test_views.py`.

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn engine() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE employees (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            dept TEXT,
            salary REAL
        )",
    );
    exec(
        &engine,
        "INSERT INTO employees (id, name, dept, salary) VALUES
            (1, 'Alice', 'eng', 90000),
            (2, 'Bob', 'mkt', 75000),
            (3, 'Carol', 'eng', 85000),
            (4, 'Dave', 'sales', 70000),
            (5, 'Eve', 'eng', 95000),
            (6, 'Frank', 'mkt', 80000)",
    );
    engine
}

fn names(result: &SQLResult) -> Vec<String> {
    result
        .rows
        .iter()
        .map(|row| match &row["name"] {
            Value::Str(s) => s.clone(),
            other => panic!("expected name string, got {other:?}"),
        })
        .collect()
}

#[test]
fn create_view_basic() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW eng_employees AS
         SELECT name, salary FROM employees WHERE dept = 'eng'",
    );
    assert!(engine.view("eng_employees").is_some());
}

#[test]
fn create_view_duplicate_raises() {
    let engine = engine();
    exec(&engine, "CREATE VIEW v AS SELECT name FROM employees");
    assert!(engine
        .sql("CREATE VIEW v AS SELECT name FROM employees", &[])
        .is_err());
}

#[test]
fn create_view_name_conflicts_with_table() {
    let engine = engine();
    assert!(engine
        .sql("CREATE VIEW employees AS SELECT name FROM employees", &[])
        .is_err());
}

#[test]
fn select_all_from_view() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW eng AS SELECT name, salary FROM employees WHERE dept = 'eng'",
    );
    let r = exec(&engine, "SELECT name FROM eng ORDER BY name");
    assert_eq!(names(&r), vec!["Alice", "Carol", "Eve"]);
}

#[test]
fn view_with_filter() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW high_sal AS
         SELECT name, salary FROM employees WHERE salary > 80000",
    );
    let r = exec(&engine, "SELECT name FROM high_sal WHERE salary > 90000");
    assert_eq!(names(&r), vec!["Eve"]);
}

#[test]
fn view_with_aggregate() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW dept_stats AS
         SELECT dept, COUNT(*) AS cnt, AVG(salary) AS avg_sal
         FROM employees GROUP BY dept",
    );
    let r = exec(&engine, "SELECT dept, cnt FROM dept_stats ORDER BY dept");
    assert_eq!(r.rows[0]["dept"], Value::Str("eng".into()));
    assert_eq!(r.rows[0]["cnt"], Value::Int(3));
}

#[test]
fn view_with_order_and_limit() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW ranked AS
         SELECT name, salary FROM employees ORDER BY salary DESC",
    );
    let r = exec(&engine, "SELECT name FROM ranked LIMIT 3");
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0]["name"], Value::Str("Eve".into()));
}

#[test]
fn view_preserves_column_types() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name, salary FROM employees",
    );
    let r = exec(&engine, "SELECT salary FROM v WHERE name = 'Alice'");
    assert_eq!(r.rows[0]["salary"], Value::Float(90000.0));
}

#[test]
fn view_with_distinct() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW depts AS SELECT DISTINCT dept FROM employees",
    );
    let r = exec(&engine, "SELECT dept FROM depts ORDER BY dept");
    let got: Vec<_> = r
        .rows
        .iter()
        .map(|row| match &row["dept"] {
            Value::Str(s) => s.clone(),
            other => panic!("expected dept string, got {other:?}"),
        })
        .collect();
    assert_eq!(got, vec!["eng", "mkt", "sales"]);
}

#[test]
fn view_does_not_leak_temp_table() {
    let engine = engine();
    exec(&engine, "CREATE VIEW v AS SELECT name FROM employees");
    exec(&engine, "SELECT name FROM v");
    assert!(!engine.has_table("v"));
    assert!(engine.view("v").is_some());
}

#[test]
fn view_does_not_shadow_real_table() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name FROM employees LIMIT 1",
    );
    exec(&engine, "SELECT name FROM v");
    let r = exec(&engine, "SELECT COUNT(*) AS cnt FROM employees");
    assert_eq!(r.rows[0]["cnt"], Value::Int(6));
}

#[test]
fn multiple_view_queries() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name, salary FROM employees",
    );
    let r1 = exec(&engine, "SELECT COUNT(*) AS cnt FROM v");
    let r2 = exec(&engine, "SELECT name FROM v WHERE salary > 90000");
    assert_eq!(r1.rows[0]["cnt"], Value::Int(6));
    assert_eq!(names(&r2), vec!["Eve"]);
}

#[test]
fn drop_view() {
    let engine = engine();
    exec(&engine, "CREATE VIEW v AS SELECT name FROM employees");
    exec(&engine, "DROP VIEW v");
    assert!(engine.view("v").is_none());
}

#[test]
fn drop_view_if_exists() {
    let engine = engine();
    exec(&engine, "DROP VIEW IF EXISTS nonexistent");
}

#[test]
fn drop_view_nonexistent_raises() {
    let engine = engine();
    assert!(engine.sql("DROP VIEW nonexistent", &[]).is_err());
}

#[test]
fn drop_view_then_select_raises() {
    let engine = engine();
    exec(&engine, "CREATE VIEW v AS SELECT name FROM employees");
    exec(&engine, "DROP VIEW v");
    assert!(engine.sql("SELECT name FROM v", &[]).is_err());
}

#[test]
fn recreate_view_after_drop() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name FROM employees WHERE dept = 'eng'",
    );
    exec(&engine, "DROP VIEW v");
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name FROM employees WHERE dept = 'mkt'",
    );
    let r = exec(&engine, "SELECT name FROM v ORDER BY name");
    assert_eq!(names(&r), vec!["Bob", "Frank"]);
}

#[test]
fn view_reflects_data_changes() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW v AS SELECT name, salary FROM employees",
    );
    exec(
        &engine,
        "INSERT INTO employees (id, name, dept, salary) VALUES (7, 'Grace', 'eng', 100000)",
    );
    let r = exec(&engine, "SELECT COUNT(*) AS cnt FROM v");
    assert_eq!(r.rows[0]["cnt"], Value::Int(7));
}

#[test]
fn view_with_window_function() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW ranked AS
         SELECT name, salary, ROW_NUMBER() OVER (ORDER BY salary DESC) AS rn
         FROM employees",
    );
    let r = exec(
        &engine,
        "SELECT name, rn FROM ranked WHERE rn <= 3 ORDER BY rn",
    );
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[0]["name"], Value::Str("Eve".into()));
}

#[test]
fn view_used_in_subquery() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW eng_ids AS SELECT id FROM employees WHERE dept = 'eng'",
    );
    let r = exec(
        &engine,
        "SELECT name FROM employees
         WHERE id IN (SELECT id FROM eng_ids)
         ORDER BY name",
    );
    assert_eq!(names(&r), vec!["Alice", "Carol", "Eve"]);
}

#[test]
fn view_of_view() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW high_sal AS
         SELECT name, salary FROM employees WHERE salary > 80000",
    );
    exec(
        &engine,
        "CREATE VIEW very_high AS SELECT name FROM high_sal WHERE salary > 90000",
    );
    let r = exec(&engine, "SELECT name FROM very_high");
    assert_eq!(names(&r), vec!["Eve"]);
}

#[test]
fn cte_and_view_together() {
    let engine = engine();
    exec(
        &engine,
        "CREATE VIEW eng AS SELECT name, salary FROM employees WHERE dept = 'eng'",
    );
    let r = exec(
        &engine,
        "WITH top AS (SELECT name FROM eng WHERE salary > 90000)
         SELECT name FROM top",
    );
    assert_eq!(names(&r), vec!["Eve"]);
}
