//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Additional SQL `UPDATE` and `DELETE` coverage.

use uqa_core::Value;
use uqa_engine::Engine;

fn products_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE products (\
             id INTEGER PRIMARY KEY, \
             name TEXT NOT NULL, \
             price REAL, \
             quantity INTEGER, \
             category TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX idx_products_gin ON products USING gin (name, category)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO products (id, name, price, quantity, category) VALUES \
             (1, 'Widget', 10.50, 100, 'tools'), \
             (2, 'Gadget', 25.00, 50, 'electronics'), \
             (3, 'Doohickey', 5.75, 200, NULL)",
            &[],
        )
        .unwrap();
    engine
}

fn users_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO users (id, name, age) VALUES \
             (1, 'Alice', 30), (2, 'Bob', 25), (3, 'Carol', 40)",
            &[],
        )
        .unwrap();
    engine
}

fn get_int(row: &uqa_sql::ResultRow, column: &str) -> i64 {
    match row.get(column) {
        Some(Value::Int(value)) => *value,
        other => panic!("expected integer column {column}, got {other:?}"),
    }
}

fn get_float(row: &uqa_sql::ResultRow, column: &str) -> f64 {
    match row.get(column) {
        Some(Value::Float(value)) => *value,
        Some(Value::Int(value)) => *value as f64,
        other => panic!("expected numeric column {column}, got {other:?}"),
    }
}

fn get_str<'a>(row: &'a uqa_sql::ResultRow, column: &str) -> &'a str {
    match row.get(column) {
        Some(Value::Str(value)) => value,
        other => panic!("expected string column {column}, got {other:?}"),
    }
}

#[test]
fn update_basic_and_expression_cases() {
    let engine = products_engine();
    let result = engine
        .sql("UPDATE products SET price = 12.00 WHERE id = 1", &[])
        .unwrap();
    assert_eq!(result.affected_rows, 1);
    let row = engine
        .sql("SELECT price FROM products WHERE id = 1", &[])
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(get_float(&row, "price"), 12.0);

    engine
        .sql(
            "UPDATE products SET category = \
             CASE WHEN price > 20 THEN 'premium' ELSE 'standard' END",
            &[],
        )
        .unwrap();
    let rows = engine
        .sql("SELECT id, category FROM products ORDER BY id", &[])
        .unwrap()
        .rows;
    assert_eq!(get_str(&rows[0], "category"), "standard");
    assert_eq!(get_str(&rows[1], "category"), "premium");
    assert_eq!(get_str(&rows[2], "category"), "standard");

    engine
        .sql(
            "UPDATE products SET name = name || ' (v2)' WHERE id = 1",
            &[],
        )
        .unwrap();
    let row = engine
        .sql("SELECT name FROM products WHERE id = 1", &[])
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(get_str(&row, "name"), "Widget (v2)");
}

#[test]
fn update_where_variants_and_text_reindex() {
    let engine = products_engine();
    engine
        .sql(
            "UPDATE products SET category = COALESCE(category, 'uncategorized')",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "UPDATE products SET name = 'Super Widget' WHERE id = 1",
            &[],
        )
        .unwrap();
    let old = engine
        .sql(
            "SELECT id FROM products WHERE text_match(name, 'Widget')",
            &[],
        )
        .unwrap();
    assert!(old.rows.iter().any(|row| get_int(row, "id") == 1));
    let new = engine
        .sql(
            "SELECT id FROM products WHERE text_match(name, 'Super')",
            &[],
        )
        .unwrap();
    assert!(new.rows.iter().any(|row| get_int(row, "id") == 1));

    let result = engine
        .sql(
            "UPDATE products SET quantity = quantity + 1 \
             WHERE category IN ('tools', 'uncategorized')",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 2);
}

#[test]
fn delete_basic_where_and_text_index() {
    let engine = products_engine();
    let result = engine
        .sql("DELETE FROM products WHERE price < 20", &[])
        .unwrap();
    assert_eq!(result.affected_rows, 2);
    let count = engine
        .sql("SELECT COUNT(*) AS cnt FROM products", &[])
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(get_int(&count, "cnt"), 1);

    let result = engine
        .sql(
            "SELECT id FROM products WHERE text_match(name, 'Widget')",
            &[],
        )
        .unwrap();
    assert!(result.rows.is_empty());
}

#[test]
fn insert_returning_variants() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "INSERT INTO t (id, name, age) VALUES (1, 'Alice', 30) RETURNING *",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 1);
    assert_eq!(result.columns, vec!["id", "name", "age"]);
    assert_eq!(get_int(&result.rows[0], "id"), 1);
    assert_eq!(get_str(&result.rows[0], "name"), "Alice");
    assert_eq!(get_int(&result.rows[0], "age"), 30);

    let result = engine
        .sql(
            "INSERT INTO t (id, name, age) VALUES (2, 'Bob', 25) \
             RETURNING id AS user_id, name AS user_name",
            &[],
        )
        .unwrap();
    assert_eq!(result.columns, vec!["user_id", "user_name"]);
    assert_eq!(get_int(&result.rows[0], "user_id"), 2);
    assert_eq!(get_str(&result.rows[0], "user_name"), "Bob");
    assert!(!result.rows[0].contains_key("age"));
}

#[test]
fn insert_returning_multi_row_and_serial() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE t (id SERIAL PRIMARY KEY, name TEXT)", &[])
        .unwrap();
    let result = engine
        .sql(
            "INSERT INTO t (name) VALUES ('Alice'), ('Bob') RETURNING id, name",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 2);
    assert_eq!(result.rows.len(), 2);
    assert_eq!(get_int(&result.rows[0], "id"), 1);
    assert_eq!(get_int(&result.rows[1], "id"), 2);
}

#[test]
fn update_returning_variants() {
    let engine = users_engine();
    let result = engine
        .sql(
            "UPDATE users SET age = 31 WHERE id = 1 RETURNING id, age",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 1);
    assert_eq!(get_int(&result.rows[0], "id"), 1);
    assert_eq!(get_int(&result.rows[0], "age"), 31);

    let result = engine
        .sql(
            "UPDATE users SET age = age + 1 WHERE age < 35 RETURNING id, age",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert!(result.rows.iter().any(|row| get_int(row, "id") == 1));
    assert!(result.rows.iter().any(|row| get_int(row, "id") == 2));

    let result = engine
        .sql("UPDATE users SET age = 0 WHERE id = 999 RETURNING id", &[])
        .unwrap();
    assert_eq!(result.columns, vec!["id"]);
    assert!(result.rows.is_empty());
}

#[test]
fn delete_returning_variants() {
    let engine = users_engine();
    let result = engine
        .sql("DELETE FROM users WHERE id = 1 RETURNING *", &[])
        .unwrap();
    assert_eq!(result.affected_rows, 1);
    assert_eq!(get_int(&result.rows[0], "id"), 1);
    assert_eq!(get_str(&result.rows[0], "name"), "Alice");
    let check = engine
        .sql("SELECT COUNT(*) AS cnt FROM users WHERE id = 1", &[])
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(get_int(&check, "cnt"), 0);

    let result = engine
        .sql("DELETE FROM users WHERE age >= 30 RETURNING id, name", &[])
        .unwrap();
    let names = result
        .rows
        .iter()
        .map(|row| get_str(row, "name"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(names, std::collections::BTreeSet::from(["Carol"]));
}

#[test]
fn on_conflict_do_nothing_variants() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO t (id, name) VALUES (1, 'Alice')", &[])
        .unwrap();
    let result = engine
        .sql(
            "INSERT INTO t (id, name) VALUES (1, 'Bob') \
             ON CONFLICT (id) DO NOTHING",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 0);
    let row = engine
        .sql("SELECT name FROM t WHERE id = 1", &[])
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(get_str(&row, "name"), "Alice");

    engine
        .sql(
            "INSERT INTO t (id, name) VALUES (1, 'Dup'), (2, 'Bob') \
             ON CONFLICT DO NOTHING",
            &[],
        )
        .unwrap();
    let count = engine
        .sql("SELECT COUNT(*) AS cnt FROM t", &[])
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(get_int(&count, "cnt"), 2);
}

#[test]
fn on_conflict_do_update_variants() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT UNIQUE, name TEXT, score INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO t (id, email, name, score) \
             VALUES (1, 'a@b.com', 'Alice', 100)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "INSERT INTO t (id, email, name, score) \
             VALUES (1, 'a@b.com', 'Alicia', 200) \
             ON CONFLICT (id) DO UPDATE \
             SET name = excluded.name, score = excluded.score \
             RETURNING id, name, score",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 1);
    assert_eq!(get_str(&result.rows[0], "name"), "Alicia");
    assert_eq!(get_int(&result.rows[0], "score"), 200);

    engine
        .sql(
            "INSERT INTO t (id, email, name, score) \
             VALUES (2, 'a@b.com', 'Bob', 50) \
             ON CONFLICT (email) DO UPDATE SET name = excluded.name",
            &[],
        )
        .unwrap();
    let row = engine
        .sql("SELECT name FROM t WHERE email = 'a@b.com'", &[])
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(get_str(&row, "name"), "Bob");
    let count = engine
        .sql("SELECT COUNT(*) AS cnt FROM t", &[])
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(get_int(&count, "cnt"), 1);
}

#[test]
fn update_from_and_delete_using_returning() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE bonuses (account_id INTEGER, amount INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO accounts (id, balance) VALUES (1, 100), (2, 200)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO bonuses (account_id, amount) VALUES (1, 10), (2, -50)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "UPDATE accounts SET balance = balance + amount FROM bonuses \
             WHERE accounts.id = bonuses.account_id RETURNING id, balance",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert!(result.rows.iter().any(|row| get_int(row, "balance") == 110));
    assert!(result.rows.iter().any(|row| get_int(row, "balance") == 150));

    let result = engine
        .sql(
            "DELETE FROM accounts USING bonuses \
             WHERE accounts.id = bonuses.account_id AND amount < 0 RETURNING id",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(get_int(&result.rows[0], "id"), 2);
}
