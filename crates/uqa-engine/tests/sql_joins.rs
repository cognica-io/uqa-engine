//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL join coverage.

use std::collections::BTreeSet;

use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn query(engine: &Engine, sql: &str) -> uqa_sql::SQLResult {
    engine.sql(sql, &[]).unwrap()
}

fn engine_with_orders() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
    );
    exec(&engine, "INSERT INTO users (id, name) VALUES (1, 'Alice')");
    exec(&engine, "INSERT INTO users (id, name) VALUES (2, 'Bob')");
    exec(&engine, "INSERT INTO users (id, name) VALUES (3, 'Carol')");
    exec(
        &engine,
        "CREATE TABLE orders (oid INTEGER PRIMARY KEY, user_id INTEGER, product TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (10, 1, 'Book')",
    );
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (11, 1, 'Pen')",
    );
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (12, 2, 'Notebook')",
    );
    engine
}

fn lateral_engine() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE depts (id INT PRIMARY KEY, dept_name TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE emps (id INT PRIMARY KEY, emp_name TEXT, dept_id INT, salary INT)",
    );
    exec(&engine, "INSERT INTO depts VALUES (1, 'Engineering')");
    exec(&engine, "INSERT INTO depts VALUES (2, 'Sales')");
    exec(&engine, "INSERT INTO emps VALUES (1, 'Alice', 1, 90000)");
    exec(&engine, "INSERT INTO emps VALUES (2, 'Bob', 1, 80000)");
    exec(&engine, "INSERT INTO emps VALUES (3, 'Charlie', 2, 70000)");
    exec(&engine, "INSERT INTO emps VALUES (4, 'Diana', 2, 75000)");
    engine
}

fn str_set(result: &uqa_sql::SQLResult, column: &str) -> BTreeSet<String> {
    result
        .rows
        .iter()
        .filter_map(|r| match r.get(column) {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn inner_join_basic() {
    let engine = engine_with_orders();
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users INNER JOIN orders ON users.id = orders.user_id",
    );
    assert_eq!(result.rows.len(), 3);
    assert_eq!(
        str_set(&result, "product"),
        ["Book", "Notebook", "Pen"]
            .into_iter()
            .map(String::from)
            .collect()
    );
}

#[test]
fn comma_join_column_pruning_keeps_only_real_source_columns() {
    let engine = engine_with_orders();
    let result = query(
        &engine,
        "SELECT name, product
         FROM users, orders
         WHERE id = user_id
         ORDER BY oid",
    );

    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[0]["name"], Value::Str("Alice".into()));
    assert_eq!(result.rows[0]["product"], Value::Str("Book".into()));
    assert_eq!(result.rows[2]["name"], Value::Str("Bob".into()));
    assert_eq!(result.rows[2]["product"], Value::Str("Notebook".into()));
}

#[test]
fn inner_join_excludes_unmatched() {
    let engine = engine_with_orders();
    let result = query(
        &engine,
        "SELECT users.name \
         FROM users INNER JOIN orders ON users.id = orders.user_id",
    );
    assert!(!str_set(&result, "name").contains("Carol"));
}

#[test]
fn inner_join_uses_composite_expression_key() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE points (id INTEGER PRIMARY KEY, x REAL, y REAL)",
    );
    exec(
        &engine,
        "CREATE TABLE tiles (x INTEGER, y INTEGER, label TEXT)",
    );
    // Coordinates chosen so PostgreSQL 17 float -> int casts (round
    // half to even, not truncation) land on the intended tiles.
    exec(
        &engine,
        "INSERT INTO points (id, x, y) VALUES
            (1, 1.2, 2.4),
            (2, 4.1, 5.0),
            (3, 9.9, 9.9)",
    );
    exec(
        &engine,
        "INSERT INTO tiles (x, y, label) VALUES
            (1, 2, 'wall'),
            (4, 5, 'floor'),
            (9, 8, 'miss')",
    );

    let result = query(
        &engine,
        "SELECT p.id, t.label
         FROM points p
         JOIN tiles t
           ON t.x = CAST(p.x AS INT)
          AND t.y = CAST(p.y AS INT)
         ORDER BY p.id",
    );

    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["label"], Value::Str("wall".into()));
    assert_eq!(result.rows[1]["label"], Value::Str("floor".into()));
}

#[test]
fn left_join_uses_composite_expression_key_and_pads_unmatched() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE points (id INTEGER PRIMARY KEY, x REAL, y REAL)",
    );
    exec(
        &engine,
        "CREATE TABLE tiles (x INTEGER, y INTEGER, label TEXT)",
    );
    // Coordinates chosen so PostgreSQL 17 float -> int casts (round
    // half to even, not truncation) land on the intended tiles.
    exec(
        &engine,
        "INSERT INTO points (id, x, y) VALUES
            (1, 1.2, 2.4),
            (2, 4.1, 5.0),
            (3, 9.9, 9.9)",
    );
    exec(
        &engine,
        "INSERT INTO tiles (x, y, label) VALUES
            (1, 2, 'wall'),
            (4, 5, 'floor')",
    );

    let result = query(
        &engine,
        "SELECT p.id, t.label
         FROM points p
         LEFT JOIN tiles t
           ON t.x = CAST(p.x AS INT)
          AND t.y = CAST(p.y AS INT)
         ORDER BY p.id",
    );

    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[0]["label"], Value::Str("wall".into()));
    assert_eq!(result.rows[1]["label"], Value::Str("floor".into()));
    assert_eq!(result.rows[2]["label"], Value::Null);
}

#[test]
fn left_join_preserves_left() {
    let engine = engine_with_orders();
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users LEFT JOIN orders ON users.id = orders.user_id",
    );
    assert_eq!(result.rows.len(), 4);
    assert!(str_set(&result, "name").contains("Carol"));
}

#[test]
fn left_join_null_for_unmatched() {
    let engine = engine_with_orders();
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users LEFT JOIN orders ON users.id = orders.user_id",
    );
    let carol: Vec<_> = result
        .rows
        .iter()
        .filter(|r| r.get("name") == Some(&Value::Str("Carol".into())))
        .collect();
    assert_eq!(carol.len(), 1);
    assert_eq!(carol[0].get("product"), Some(&Value::Null));
}

#[test]
fn cross_join_cartesian() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO a (id, val) VALUES (1, 'x')");
    exec(&engine, "INSERT INTO a (id, val) VALUES (2, 'y')");
    exec(
        &engine,
        "CREATE TABLE b (id INTEGER PRIMARY KEY, label TEXT)",
    );
    exec(&engine, "INSERT INTO b (id, label) VALUES (10, 'p')");
    exec(&engine, "INSERT INTO b (id, label) VALUES (20, 'q')");
    exec(&engine, "INSERT INTO b (id, label) VALUES (30, 'r')");
    let result = query(&engine, "SELECT a.val, b.label FROM a CROSS JOIN b");
    assert_eq!(result.rows.len(), 6);
}

#[test]
fn cross_join_empty_side() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO a (id, val) VALUES (1, 'x')");
    exec(
        &engine,
        "CREATE TABLE b (id INTEGER PRIMARY KEY, label TEXT)",
    );
    let result = query(&engine, "SELECT * FROM a CROSS JOIN b");
    assert_eq!(result.rows.len(), 0);
}

#[test]
fn right_join_preserves_right() {
    let engine = engine_with_orders();
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (13, 99, 'Ghost')",
    );
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users RIGHT JOIN orders ON users.id = orders.user_id",
    );
    assert!(str_set(&result, "product").contains("Ghost"));
    assert_eq!(result.rows.len(), 4);
}

#[test]
fn right_join_null_for_unmatched_left() {
    let engine = engine_with_orders();
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (13, 99, 'Ghost')",
    );
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users RIGHT JOIN orders ON users.id = orders.user_id",
    );
    let ghost: Vec<_> = result
        .rows
        .iter()
        .filter(|r| r.get("product") == Some(&Value::Str("Ghost".into())))
        .collect();
    assert_eq!(ghost.len(), 1);
    assert_eq!(ghost[0].get("name"), Some(&Value::Null));
}

#[test]
fn full_join_preserves_both() {
    let engine = engine_with_orders();
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (13, 99, 'Ghost')",
    );
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users FULL OUTER JOIN orders ON users.id = orders.user_id",
    );
    assert_eq!(result.rows.len(), 5);
    assert!(str_set(&result, "name").contains("Carol"));
    assert!(str_set(&result, "product").contains("Ghost"));
}

#[test]
fn full_join_no_overlap() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO a (id, val) VALUES (1, 'x')");
    exec(&engine, "CREATE TABLE b (id INTEGER PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO b (id, val) VALUES (2, 'y')");
    let result = query(&engine, "SELECT * FROM a FULL OUTER JOIN b ON a.id = b.id");
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn implicit_cross_join() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE a (id INTEGER PRIMARY KEY, val TEXT)");
    exec(&engine, "INSERT INTO a (id, val) VALUES (1, 'x')");
    exec(&engine, "INSERT INTO a (id, val) VALUES (2, 'y')");
    exec(
        &engine,
        "CREATE TABLE b (id INTEGER PRIMARY KEY, label TEXT)",
    );
    exec(&engine, "INSERT INTO b (id, label) VALUES (10, 'p')");
    exec(&engine, "INSERT INTO b (id, label) VALUES (20, 'q')");
    let result = query(&engine, "SELECT a.val, b.label FROM a, b");
    assert_eq!(result.rows.len(), 4);
}

#[test]
fn implicit_cross_join_with_where() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
    );
    exec(&engine, "INSERT INTO users (id, name) VALUES (1, 'Alice')");
    exec(&engine, "INSERT INTO users (id, name) VALUES (2, 'Bob')");
    exec(
        &engine,
        "CREATE TABLE orders (oid INTEGER PRIMARY KEY, user_id INTEGER, product TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (10, 1, 'Book')",
    );
    exec(
        &engine,
        "INSERT INTO orders (oid, user_id, product) VALUES (11, 2, 'Pen')",
    );
    let result = query(
        &engine,
        "SELECT users.name, orders.product \
         FROM users, orders WHERE users.id = orders.user_id",
    );
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn three_table_cross_join() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE a (id INTEGER PRIMARY KEY, x TEXT)");
    exec(&engine, "INSERT INTO a (id, x) VALUES (1, 'a')");
    exec(&engine, "CREATE TABLE b (id INTEGER PRIMARY KEY, y TEXT)");
    exec(&engine, "INSERT INTO b (id, y) VALUES (1, 'b')");
    exec(&engine, "CREATE TABLE c (id INTEGER PRIMARY KEY, z TEXT)");
    exec(&engine, "INSERT INTO c (id, z) VALUES (1, 'c')");
    exec(&engine, "INSERT INTO c (id, z) VALUES (2, 'd')");
    let result = query(&engine, "SELECT a.x, b.y, c.z FROM a, b, c");
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn lateral_subquery_with_aggregate() {
    let engine = lateral_engine();
    let result = query(
        &engine,
        "SELECT d.dept_name, sub.top_salary \
         FROM depts d, \
         LATERAL (SELECT MAX(salary) AS top_salary \
         FROM emps WHERE emps.dept_id = d.id) sub \
         ORDER BY d.dept_name",
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        result.rows[0]["dept_name"],
        Value::Str("Engineering".into())
    );
    assert_eq!(result.rows[0]["top_salary"], Value::Int(90000));
    assert_eq!(result.rows[1]["dept_name"], Value::Str("Sales".into()));
    assert_eq!(result.rows[1]["top_salary"], Value::Int(75000));
}

#[test]
fn lateral_with_limit() {
    let engine = lateral_engine();
    let result = query(
        &engine,
        "SELECT d.dept_name, sub.top_emp, sub.top_sal \
         FROM depts d, \
         LATERAL (SELECT emp_name AS top_emp, salary AS top_sal \
         FROM emps WHERE emps.dept_id = d.id \
         ORDER BY salary DESC LIMIT 1) sub \
         ORDER BY d.dept_name",
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["top_emp"], Value::Str("Alice".into()));
    assert_eq!(result.rows[0]["top_sal"], Value::Int(90000));
    assert_eq!(result.rows[1]["top_emp"], Value::Str("Diana".into()));
    assert_eq!(result.rows[1]["top_sal"], Value::Int(75000));
}

#[test]
fn lateral_with_count() {
    let engine = lateral_engine();
    let result = query(
        &engine,
        "SELECT d.dept_name, sub.emp_count \
         FROM depts d, \
         LATERAL (SELECT COUNT(*) AS emp_count \
         FROM emps WHERE emps.dept_id = d.id) sub \
         ORDER BY d.dept_name",
    );
    assert_eq!(
        result.rows[0]["dept_name"],
        Value::Str("Engineering".into())
    );
    assert_eq!(result.rows[0]["emp_count"], Value::Int(2));
    assert_eq!(result.rows[1]["dept_name"], Value::Str("Sales".into()));
    assert_eq!(result.rows[1]["emp_count"], Value::Int(2));
}
