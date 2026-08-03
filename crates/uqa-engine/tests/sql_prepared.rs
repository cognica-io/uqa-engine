//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared-statement coverage.

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn err(engine: &Engine, sql: &str) -> String {
    engine.sql(sql, &[]).unwrap_err().to_string()
}

fn setup() -> Engine {
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
            (5, 'Eve', 'eng', 95000)",
    );
    engine
}

#[test]
fn prepare_select_registers_statement() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE get_by_id (INTEGER) AS SELECT name FROM employees WHERE id = $1",
    );
    assert!(engine.lookup_prepared("get_by_id").is_some());
}

#[test]
fn prepare_duplicate_raises() {
    let engine = setup();
    exec(&engine, "PREPARE q AS SELECT name FROM employees");
    assert!(err(&engine, "PREPARE q AS SELECT name FROM employees").contains("already exists"));
}

#[test]
fn prepare_insert_registers_statement() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE ins AS
         INSERT INTO employees (id, name, dept, salary)
         VALUES ($1, $2, $3, $4)",
    );
    assert!(engine.lookup_prepared("ins").is_some());
}

#[test]
fn prepare_update_registers_statement() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE upd AS UPDATE employees SET salary = $1 WHERE id = $2",
    );
    assert!(engine.lookup_prepared("upd").is_some());
}

#[test]
fn prepare_delete_registers_statement() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE del AS DELETE FROM employees WHERE id = $1",
    );
    assert!(engine.lookup_prepared("del").is_some());
}

#[test]
fn execute_select_single_param() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE get_by_id AS SELECT name FROM employees WHERE id = $1",
    );
    let result = exec(&engine, "EXECUTE get_by_id (1)");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["name"], Value::Str("Alice".into()));
}

#[test]
fn execute_select_different_params() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE get_by_id AS SELECT name FROM employees WHERE id = $1",
    );
    assert_eq!(
        exec(&engine, "EXECUTE get_by_id (1)").rows[0]["name"],
        Value::Str("Alice".into())
    );
    assert_eq!(
        exec(&engine, "EXECUTE get_by_id (3)").rows[0]["name"],
        Value::Str("Carol".into())
    );
}

#[test]
fn execute_select_multiple_params() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE get_by_dept_sal AS
         SELECT name FROM employees
         WHERE dept = $1 AND salary > $2
         ORDER BY name",
    );
    let result = exec(&engine, "EXECUTE get_by_dept_sal ('eng', 87000)");
    let names: Vec<_> = result.rows.iter().map(|r| r["name"].clone()).collect();
    assert_eq!(
        names,
        vec![Value::Str("Alice".into()), Value::Str("Eve".into())]
    );
}

#[test]
fn execute_insert() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE ins AS
         INSERT INTO employees (id, name, dept, salary)
         VALUES ($1, $2, $3, $4)",
    );
    exec(&engine, "EXECUTE ins (6, 'Frank', 'mkt', 80000)");
    let result = exec(&engine, "SELECT name FROM employees WHERE id = 6");
    assert_eq!(result.rows[0]["name"], Value::Str("Frank".into()));
}

#[test]
fn execute_update() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE upd AS UPDATE employees SET salary = $1 WHERE id = $2",
    );
    exec(&engine, "EXECUTE upd (100000, 1)");
    let result = exec(&engine, "SELECT salary FROM employees WHERE id = 1");
    assert_eq!(result.rows[0]["salary"], Value::Float(100_000.0));
}

#[test]
fn execute_delete() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE del AS DELETE FROM employees WHERE id = $1",
    );
    exec(&engine, "EXECUTE del (4)");
    let result = exec(&engine, "SELECT COUNT(*) AS cnt FROM employees");
    assert_eq!(result.rows[0]["cnt"], Value::Int(4));
}

#[test]
fn execute_nonexistent_raises() {
    let engine = setup();
    assert!(err(&engine, "EXECUTE nonexistent (1)").contains("does not exist"));
}

#[test]
fn execute_missing_param_raises() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE q AS SELECT name FROM employees WHERE id = $1 AND dept = $2",
    );
    assert!(err(&engine, "EXECUTE q (1)").contains("No value supplied"));
}

#[test]
fn execute_reusable() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE get_name AS SELECT name FROM employees WHERE id = $1",
    );
    let mut names = Vec::new();
    for i in 1..=5 {
        let result = exec(&engine, &format!("EXECUTE get_name ({i})"));
        names.push(result.rows[0]["name"].clone());
    }
    assert_eq!(
        names,
        vec![
            Value::Str("Alice".into()),
            Value::Str("Bob".into()),
            Value::Str("Carol".into()),
            Value::Str("Dave".into()),
            Value::Str("Eve".into()),
        ]
    );
}

#[test]
fn deallocate_removes_statement() {
    let engine = setup();
    exec(&engine, "PREPARE q AS SELECT name FROM employees");
    exec(&engine, "DEALLOCATE q");
    assert!(engine.lookup_prepared("q").is_none());
}

#[test]
fn deallocate_nonexistent_raises() {
    let engine = setup();
    assert!(err(&engine, "DEALLOCATE nonexistent").contains("does not exist"));
}

#[test]
fn deallocate_all_removes_every_statement() {
    let engine = setup();
    exec(&engine, "PREPARE q1 AS SELECT name FROM employees");
    exec(&engine, "PREPARE q2 AS SELECT dept FROM employees");
    exec(&engine, "DEALLOCATE ALL");
    assert!(engine.lookup_prepared("q1").is_none());
    assert!(engine.lookup_prepared("q2").is_none());
}

#[test]
fn execute_after_deallocate_raises() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE q AS SELECT name FROM employees WHERE id = $1",
    );
    exec(&engine, "DEALLOCATE q");
    assert!(err(&engine, "EXECUTE q (1)").contains("does not exist"));
}

#[test]
fn reprepare_after_deallocate() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE q AS SELECT name FROM employees WHERE dept = $1",
    );
    exec(&engine, "DEALLOCATE q");
    exec(
        &engine,
        "PREPARE q AS SELECT salary FROM employees WHERE id = $1",
    );
    let result = exec(&engine, "EXECUTE q (1)");
    assert_eq!(result.rows[0]["salary"], Value::Float(90_000.0));
}

#[test]
fn prepare_with_typed_params() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE q (INTEGER) AS SELECT name FROM employees WHERE id = $1",
    );
    let result = exec(&engine, "EXECUTE q (2)");
    assert_eq!(result.rows[0]["name"], Value::Str("Bob".into()));
}

#[test]
fn prepare_select_with_order_and_limit() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE top_earners AS
         SELECT name, salary FROM employees
         WHERE dept = $1 ORDER BY salary DESC LIMIT 2",
    );
    let result = exec(&engine, "EXECUTE top_earners ('eng')");
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["name"], Value::Str("Eve".into()));
    assert_eq!(result.rows[1]["name"], Value::Str("Alice".into()));
}

#[test]
fn prepare_select_no_params() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE all_names AS SELECT name FROM employees ORDER BY name",
    );
    let result = exec(&engine, "EXECUTE all_names");
    let names: Vec<_> = result.rows.iter().map(|r| r["name"].clone()).collect();
    assert_eq!(
        names,
        vec![
            Value::Str("Alice".into()),
            Value::Str("Bob".into()),
            Value::Str("Carol".into()),
            Value::Str("Dave".into()),
            Value::Str("Eve".into()),
        ]
    );
}

#[test]
fn prepare_with_null_param() {
    let engine = setup();
    exec(
        &engine,
        "INSERT INTO employees (id, name, dept, salary) VALUES (6, 'Frank', NULL, 80000)",
    );
    exec(
        &engine,
        "PREPARE get_null_dept AS SELECT name FROM employees WHERE dept IS NULL",
    );
    let result = exec(&engine, "EXECUTE get_null_dept");
    assert_eq!(result.rows[0]["name"], Value::Str("Frank".into()));
}

#[test]
fn multiple_prepared_coexist() {
    let engine = setup();
    exec(
        &engine,
        "PREPARE by_id AS SELECT name FROM employees WHERE id = $1",
    );
    exec(
        &engine,
        "PREPARE by_dept AS
         SELECT name FROM employees WHERE dept = $1 ORDER BY name",
    );
    let r1 = exec(&engine, "EXECUTE by_id (1)");
    let r2 = exec(&engine, "EXECUTE by_dept ('eng')");
    assert_eq!(r1.rows[0]["name"], Value::Str("Alice".into()));
    let dept_names: Vec<_> = r2.rows.iter().map(|r| r["name"].clone()).collect();
    assert_eq!(
        dept_names,
        vec![
            Value::Str("Alice".into()),
            Value::Str("Carol".into()),
            Value::Str("Eve".into()),
        ]
    );
}
