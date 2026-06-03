//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `test_aggregates`. Covers `COUNT DISTINCT`,
//! `STRING_AGG` / `ARRAY_AGG` with `DISTINCT` and `ORDER BY`, `BOOL_AND` /
//! `BOOL_OR`, `FILTER (WHERE ...)`, `GROUP BY` by ordinal / alias,
//! complex `HAVING`, `NUMERIC` precision/scale, `STDDEV` / `VARIANCE`,
//! `PERCENTILE_CONT` / `PERCENTILE_DISC`, and `MODE`.

use uqa_core::Value;
use uqa_engine::Engine;

fn engine() -> Engine {
    Engine::new()
}

fn engine_with_data() -> Engine {
    let eng = engine();
    eng.sql(
        "CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO users (id, name, age) VALUES \
         (1, 'Alice', 30), (2, 'Bob', 25), (3, 'Carol', 35), (4, 'Dave', 25)",
        &[],
    )
    .unwrap();
    eng
}

fn engine_with_table() -> Engine {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO t (id, val, name) VALUES \
         (1, 10, 'alpha'), (2, 20, 'bravo'), (3, 30, 'charlie')",
        &[],
    )
    .unwrap();
    eng
}

fn engine_with_products() -> Engine {
    let eng = engine();
    eng.sql(
        "CREATE TABLE products ( \
            id INTEGER PRIMARY KEY, category TEXT, name TEXT, \
            price INTEGER, active BOOLEAN \
         )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO products (id, category, name, price, active) VALUES \
         (1, 'fruit', 'Apple', 3, true), \
         (2, 'fruit', 'Banana', 2, true), \
         (3, 'fruit', 'Cherry', 5, false), \
         (4, 'veggie', 'Daikon', 4, true), \
         (5, 'veggie', 'Eggplant', 6, false)",
        &[],
    )
    .unwrap();
    eng
}

fn engine_with_large_numbers(n: i64) -> Engine {
    let eng = engine();
    eng.sql("CREATE TABLE big_numbers (n INTEGER)", &[])
        .unwrap();
    let values = (0..n)
        .map(|n| format!("({n})"))
        .collect::<Vec<_>>()
        .join(", ");
    eng.sql(&format!("INSERT INTO big_numbers (n) VALUES {values}"), &[])
        .unwrap();
    eng
}

fn int_col(row: &uqa_sql::ResultRow, col: &str) -> Option<i64> {
    match row.get(col)? {
        Value::Int(n) => Some(*n),
        _ => None,
    }
}

fn float_col(row: &uqa_sql::ResultRow, col: &str) -> Option<f64> {
    match row.get(col)? {
        Value::Float(f) => Some(*f),
        Value::Int(n) => Some(*n as f64),
        _ => None,
    }
}

fn str_col<'a>(row: &'a uqa_sql::ResultRow, col: &str) -> Option<&'a str> {
    match row.get(col)? {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

fn list_col<'a>(row: &'a uqa_sql::ResultRow, col: &str) -> Option<&'a Vec<Value>> {
    match row.get(col)? {
        Value::List(v) => Some(v),
        _ => None,
    }
}

fn bool_col(row: &uqa_sql::ResultRow, col: &str) -> Option<bool> {
    match row.get(col)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

// =====================================================================
// COUNT(DISTINCT)
// =====================================================================

#[test]
fn count_distinct_basic() {
    let eng = engine_with_data();
    let r = eng
        .sql("SELECT COUNT(DISTINCT age) AS cnt FROM users", &[])
        .unwrap();
    assert_eq!(int_col(&r.rows[0], "cnt"), Some(3));
}

#[test]
fn aggregates_can_be_nested_inside_projection_expressions() {
    let eng = engine_with_data();
    let r = eng
        .sql(
            "SELECT 'users' AS label, COUNT(*) = 4 AS count_ok, SUM(age) > 100 AS sum_ok FROM users",
            &[],
        )
        .unwrap();
    assert_eq!(str_col(&r.rows[0], "label"), Some("users"));
    assert_eq!(bool_col(&r.rows[0], "count_ok"), Some(true));
    assert_eq!(bool_col(&r.rows[0], "sum_ok"), Some(true));
}

#[test]
fn count_distinct_with_group_by() {
    let eng = engine();
    eng.sql("CREATE TABLE sales (dept TEXT, product TEXT)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO sales (dept, product) VALUES \
         ('A', 'x'), ('A', 'x'), ('A', 'y'), ('B', 'z')",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT dept, COUNT(DISTINCT product) AS cnt FROM sales GROUP BY dept ORDER BY dept",
            &[],
        )
        .unwrap();
    assert_eq!(int_col(&r.rows[0], "cnt"), Some(2));
    assert_eq!(int_col(&r.rows[1], "cnt"), Some(1));
}

#[test]
fn count_distinct_skips_nulls() {
    let eng = engine();
    eng.sql("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO t (id, val) VALUES (1, 'a'), (2, 'a'); \
         INSERT INTO t (id) VALUES (3)",
        &[],
    )
    .ok();
    eng.sql("INSERT INTO t (id, val) VALUES (1, 'a')", &[]).ok();
    eng.sql("INSERT INTO t (id, val) VALUES (2, 'a')", &[]).ok();
    eng.sql("INSERT INTO t (id) VALUES (3)", &[]).ok();
    let r = eng
        .sql("SELECT COUNT(DISTINCT val) AS cnt FROM t", &[])
        .unwrap();
    assert_eq!(int_col(&r.rows[0], "cnt"), Some(1));
}

// =====================================================================
// STRING_AGG
// =====================================================================

#[test]
fn string_agg_basic() {
    let eng = engine_with_data();
    let r = eng
        .sql("SELECT STRING_AGG(name, ', ') AS names FROM users", &[])
        .unwrap();
    let names = str_col(&r.rows[0], "names").unwrap();
    assert!(names.contains("Alice"));
    assert!(names.contains("Bob"));
    assert!(names.contains(", "));
}

#[test]
fn string_agg_with_group_by() {
    let eng = engine();
    eng.sql("CREATE TABLE items (category TEXT, name TEXT)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO items (category, name) VALUES \
         ('fruit', 'apple'), ('fruit', 'banana'), ('veggie', 'carrot')",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT category, STRING_AGG(name, ',') AS items \
             FROM items GROUP BY category ORDER BY category",
            &[],
        )
        .unwrap();
    let first = str_col(&r.rows[0], "items").unwrap();
    assert!(first == "apple,banana" || first == "banana,apple");
    assert_eq!(str_col(&r.rows[1], "items"), Some("carrot"));
}

#[test]
fn string_agg_all_null_yields_null() {
    let eng = engine();
    eng.sql("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id) VALUES (1), (2)", &[]).ok();
    eng.sql("INSERT INTO t (id) VALUES (1)", &[]).ok();
    eng.sql("INSERT INTO t (id) VALUES (2)", &[]).ok();
    let r = eng
        .sql("SELECT STRING_AGG(val, ',') AS vals FROM t", &[])
        .unwrap();
    assert!(matches!(r.rows[0].get("vals"), Some(Value::Null) | None));
}

#[test]
fn string_agg_custom_delimiter() {
    let eng = engine_with_data();
    let r = eng
        .sql("SELECT STRING_AGG(name, ' | ') AS names FROM users", &[])
        .unwrap();
    let names = str_col(&r.rows[0], "names").unwrap();
    assert!(names.contains(" | "));
}

#[test]
fn string_agg_distinct() {
    let eng = engine();
    eng.sql("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO t (id, val) VALUES (1, 'a'), (2, 'b'), (3, 'a'), (4, 'c')",
        &[],
    )
    .unwrap();
    let r = eng
        .sql("SELECT STRING_AGG(DISTINCT val, ',') AS vals FROM t", &[])
        .unwrap();
    let vals = str_col(&r.rows[0], "vals").unwrap();
    let parts: std::collections::BTreeSet<&str> = vals.split(',').collect();
    assert!(parts.contains("a"));
    assert!(parts.contains("b"));
    assert!(parts.contains("c"));
}

// =====================================================================
// ARRAY_AGG
// =====================================================================

#[test]
fn array_agg_basic() {
    let eng = engine_with_products();
    let r = eng
        .sql("SELECT array_agg(name) AS names FROM products", &[])
        .unwrap();
    let names = list_col(&r.rows[0], "names").unwrap();
    let strs: std::collections::BTreeSet<String> = names
        .iter()
        .filter_map(|v| match v {
            Value::Str(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    for expected in ["Apple", "Banana", "Cherry", "Daikon", "Eggplant"] {
        assert!(strs.contains(expected), "{expected} missing in {strs:?}");
    }
}

#[test]
fn array_agg_with_group_by() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT category, array_agg(name) AS names FROM products GROUP BY category",
            &[],
        )
        .unwrap();
    let mut by_cat: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for row in &r.rows {
        let cat = str_col(row, "category").unwrap_or_default().to_string();
        let names = list_col(row, "names").unwrap();
        let s: std::collections::BTreeSet<String> = names
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        by_cat.insert(cat, s);
    }
    let fruit = by_cat.get("fruit").unwrap();
    assert!(fruit.contains("Apple"));
    assert!(fruit.contains("Banana"));
    assert!(fruit.contains("Cherry"));
    let veggie = by_cat.get("veggie").unwrap();
    assert!(veggie.contains("Daikon"));
    assert!(veggie.contains("Eggplant"));
}

#[test]
fn array_agg_with_order_by() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT array_agg(name ORDER BY name) AS names FROM products",
            &[],
        )
        .unwrap();
    let names = list_col(&r.rows[0], "names").unwrap();
    let strs: Vec<String> = names
        .iter()
        .filter_map(|v| match v {
            Value::Str(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        strs,
        vec!["Apple", "Banana", "Cherry", "Daikon", "Eggplant"]
    );
}

#[test]
fn array_agg_with_order_by_desc() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT array_agg(name ORDER BY name DESC) AS names FROM products",
            &[],
        )
        .unwrap();
    let names = list_col(&r.rows[0], "names").unwrap();
    let strs: Vec<String> = names
        .iter()
        .filter_map(|v| match v {
            Value::Str(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        strs,
        vec!["Eggplant", "Daikon", "Cherry", "Banana", "Apple"]
    );
}

#[test]
fn aggregate_value_buffer_spills_and_restores_ordered_and_unordered_values() {
    let eng = engine_with_large_numbers(5000);
    let r = eng
        .sql(
            "SELECT array_agg(n ORDER BY n DESC) AS nums, \
                    var_pop(n) AS variance, \
                    sum(n) AS total \
             FROM big_numbers",
            &[],
        )
        .unwrap();

    let nums = list_col(&r.rows[0], "nums").unwrap();
    assert_eq!(nums.len(), 5000);
    assert_eq!(nums.first(), Some(&Value::Int(4999)));
    assert_eq!(nums.get(4095), Some(&Value::Int(904)));
    assert_eq!(nums.last(), Some(&Value::Int(0)));
    assert_eq!(int_col(&r.rows[0], "total"), Some(12_497_500));
    let variance = float_col(&r.rows[0], "variance").unwrap();
    assert!((variance - 2_083_333.25).abs() < 0.001);
}

// =====================================================================
// BOOL_AND
// =====================================================================

#[test]
fn bool_and_all_true() {
    let eng = engine();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, flag BOOLEAN)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id, flag) VALUES (1, true), (2, true)", &[])
        .unwrap();
    let r = eng
        .sql("SELECT bool_and(flag) AS result FROM t", &[])
        .unwrap();
    assert_eq!(bool_col(&r.rows[0], "result"), Some(true));
}

#[test]
fn bool_and_mixed_returns_false() {
    let eng = engine_with_products();
    let r = eng
        .sql("SELECT bool_and(active) AS result FROM products", &[])
        .unwrap();
    assert_eq!(bool_col(&r.rows[0], "result"), Some(false));
}

#[test]
fn bool_and_with_group_by() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT category, bool_and(active) AS all_active FROM products GROUP BY category",
            &[],
        )
        .unwrap();
    let mut by_cat: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    for row in &r.rows {
        let cat = str_col(row, "category").unwrap_or_default().to_string();
        if let Some(b) = bool_col(row, "all_active") {
            by_cat.insert(cat, b);
        }
    }
    assert_eq!(by_cat.get("fruit").copied(), Some(false));
    assert_eq!(by_cat.get("veggie").copied(), Some(false));
}

// =====================================================================
// BOOL_OR
// =====================================================================

#[test]
fn bool_or_all_false() {
    let eng = engine();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, flag BOOLEAN)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO t (id, flag) VALUES (1, false), (2, false)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql("SELECT bool_or(flag) AS result FROM t", &[])
        .unwrap();
    assert_eq!(bool_col(&r.rows[0], "result"), Some(false));
}

#[test]
fn bool_or_mixed_returns_true() {
    let eng = engine_with_products();
    let r = eng
        .sql("SELECT bool_or(active) AS result FROM products", &[])
        .unwrap();
    assert_eq!(bool_col(&r.rows[0], "result"), Some(true));
}

#[test]
fn bool_or_with_group_by() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT category, bool_or(active) AS any_active FROM products GROUP BY category",
            &[],
        )
        .unwrap();
    let mut by_cat: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    for row in &r.rows {
        let cat = str_col(row, "category").unwrap_or_default().to_string();
        if let Some(b) = bool_col(row, "any_active") {
            by_cat.insert(cat, b);
        }
    }
    assert_eq!(by_cat.get("fruit").copied(), Some(true));
    assert_eq!(by_cat.get("veggie").copied(), Some(true));
}

// =====================================================================
// Aggregate FILTER
// =====================================================================

#[test]
fn count_filter_active() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT COUNT(*) FILTER (WHERE active) AS active_count FROM products",
            &[],
        )
        .unwrap();
    assert_eq!(int_col(&r.rows[0], "active_count"), Some(3));
}

#[test]
fn sum_filter_active() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT SUM(price) FILTER (WHERE active) AS active_total FROM products",
            &[],
        )
        .unwrap();
    assert_eq!(int_col(&r.rows[0], "active_total"), Some(9));
}

#[test]
fn filter_with_group_by() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT category, COUNT(*) AS total, \
                    COUNT(*) FILTER (WHERE active) AS active \
             FROM products GROUP BY category",
            &[],
        )
        .unwrap();
    let mut by_cat: std::collections::BTreeMap<String, (i64, i64)> =
        std::collections::BTreeMap::new();
    for row in &r.rows {
        let cat = str_col(row, "category").unwrap_or_default().to_string();
        let total = int_col(row, "total").unwrap_or(0);
        let active = int_col(row, "active").unwrap_or(0);
        by_cat.insert(cat, (total, active));
    }
    assert_eq!(by_cat.get("fruit").copied(), Some((3, 2)));
    assert_eq!(by_cat.get("veggie").copied(), Some((2, 1)));
}

#[test]
fn filter_with_comparison() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT COUNT(*) FILTER (WHERE price > 3) AS expensive FROM products",
            &[],
        )
        .unwrap();
    assert_eq!(int_col(&r.rows[0], "expensive"), Some(3));
}

// =====================================================================
// Aggregate ORDER BY
// =====================================================================

#[test]
fn string_agg_ordered_asc() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT string_agg(name, ', ' ORDER BY name) AS names FROM products",
            &[],
        )
        .unwrap();
    assert_eq!(
        str_col(&r.rows[0], "names"),
        Some("Apple, Banana, Cherry, Daikon, Eggplant")
    );
}

#[test]
fn string_agg_ordered_desc() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT string_agg(name, ', ' ORDER BY name DESC) AS names FROM products",
            &[],
        )
        .unwrap();
    assert_eq!(
        str_col(&r.rows[0], "names"),
        Some("Eggplant, Daikon, Cherry, Banana, Apple")
    );
}

#[test]
fn array_agg_ordered_with_group_by() {
    let eng = engine_with_products();
    let r = eng
        .sql(
            "SELECT category, array_agg(name ORDER BY price DESC) AS by_price \
             FROM products GROUP BY category",
            &[],
        )
        .unwrap();
    let mut by_cat: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for row in &r.rows {
        let cat = str_col(row, "category").unwrap_or_default().to_string();
        let names = list_col(row, "by_price").unwrap();
        let s: Vec<String> = names
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        by_cat.insert(cat, s);
    }
    assert_eq!(
        by_cat.get("fruit").cloned().unwrap_or_default(),
        vec!["Cherry", "Apple", "Banana"]
    );
    assert_eq!(
        by_cat.get("veggie").cloned().unwrap_or_default(),
        vec!["Eggplant", "Daikon"]
    );
}

// =====================================================================
// GROUP BY enhanced
// =====================================================================

#[test]
fn group_by_ordinal() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE sales (id INTEGER PRIMARY KEY, region TEXT, amount INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO sales (id, region, amount) VALUES \
         (1, 'East', 100), (2, 'West', 200), (3, 'East', 150)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT region, SUM(amount) AS total FROM sales GROUP BY 1",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2);
    let mut by_region: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for row in &r.rows {
        if let (Some(reg), Some(t)) = (str_col(row, "region"), int_col(row, "total")) {
            by_region.insert(reg.to_string(), t);
        }
    }
    assert_eq!(by_region.get("East").copied(), Some(250));
    assert_eq!(by_region.get("West").copied(), Some(200));
}

#[test]
fn group_by_alias() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, category TEXT, price INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO items (id, category, price) VALUES (1, 'A', 10), (2, 'B', 20), (3, 'A', 30)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT category AS cat, COUNT(*) AS cnt FROM items GROUP BY cat",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2);
    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for row in &r.rows {
        if let (Some(c), Some(n)) = (str_col(row, "cat"), int_col(row, "cnt")) {
            counts.insert(c.to_string(), n);
        }
    }
    assert_eq!(counts.get("A").copied(), Some(2));
    assert_eq!(counts.get("B").copied(), Some(1));
}

// =====================================================================
// Complex HAVING
// =====================================================================

#[test]
fn having_with_and() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE sales (id INTEGER PRIMARY KEY, region TEXT, amount INTEGER)",
        &[],
    )
    .unwrap();
    let data = [
        ("East", 100),
        ("East", 200),
        ("East", 50),
        ("West", 300),
        ("West", 400),
        ("North", 10),
    ];
    for (i, (region, amount)) in data.iter().enumerate() {
        eng.sql(
            &format!(
                "INSERT INTO sales (id, region, amount) VALUES ({}, '{region}', {amount})",
                i + 1
            ),
            &[],
        )
        .unwrap();
    }
    let r = eng
        .sql(
            "SELECT region, COUNT(*) AS cnt, SUM(amount) AS total \
             FROM sales GROUP BY region \
             HAVING COUNT(*) > 2 AND SUM(amount) > 300",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(str_col(&r.rows[0], "region"), Some("East"));
}

#[test]
fn having_aggregate_comparison() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE scores (id INTEGER PRIMARY KEY, team TEXT, score INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO scores (id, team, score) VALUES \
         (1, 'A', 90), (2, 'A', 80), (3, 'B', 50), (4, 'B', 60)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT team, MAX(score) AS hi, MIN(score) AS lo \
             FROM scores GROUP BY team \
             HAVING MAX(score) > MIN(score) + 20",
            &[],
        )
        .unwrap();
    assert!(r.rows.is_empty());
}

#[test]
fn having_simple() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO t (id, cat, val) VALUES (1, 'a', 10), (2, 'a', 20), (3, 'b', 30)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT cat, COUNT(*) AS cnt FROM t GROUP BY cat HAVING COUNT(*) > 1",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(str_col(&r.rows[0], "cat"), Some("a"));
}

// =====================================================================
// NUMERIC(precision, scale)
// =====================================================================

#[test]
fn numeric_create_table() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, price NUMERIC(10, 2))",
        &[],
    )
    .unwrap();
    let r = eng.sql("SELECT * FROM t", &[]).unwrap();
    assert!(r.columns.iter().any(|c| c == "price"));
}

#[test]
fn numeric_insert_rounds_to_scale() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, price NUMERIC(10, 2))",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO t (id, price) VALUES (1, 19.999)", &[])
        .unwrap();
    let r = eng.sql("SELECT price FROM t WHERE id = 1", &[]).unwrap();
    let v = float_col(&r.rows[0], "price").unwrap();
    assert!((v - 20.0).abs() < 0.001);
}

#[test]
fn numeric_insert_preserves_scale() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, amount NUMERIC(8, 3))",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO t (id, amount) VALUES (1, 123.456)", &[])
        .unwrap();
    let r = eng.sql("SELECT amount FROM t WHERE id = 1", &[]).unwrap();
    let v = float_col(&r.rows[0], "amount").unwrap();
    assert!((v - 123.456).abs() < 0.0001);
}

#[test]
fn numeric_arithmetic() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a NUMERIC(10, 2), b NUMERIC(10, 2))",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO t (id, a, b) VALUES (1, 10.50, 3.25)", &[])
        .unwrap();
    let r = eng
        .sql("SELECT a + b AS total FROM t WHERE id = 1", &[])
        .unwrap();
    let v = float_col(&r.rows[0], "total").unwrap();
    assert!((v - 13.75).abs() < 0.001);
}

#[test]
fn numeric_comparison() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val NUMERIC(10, 2))",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO t (id, val) VALUES (1, 10.50), (2, 20.75), (3, 5.25)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql("SELECT id FROM t WHERE val > 10.00 ORDER BY id", &[])
        .unwrap();
    let ids: Vec<i64> = r.rows.iter().filter_map(|row| int_col(row, "id")).collect();
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn numeric_no_scale_specified() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val NUMERIC(10))",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO t (id, val) VALUES (1, 42.9)", &[])
        .unwrap();
    let r = eng.sql("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    let v = float_col(&r.rows[0], "val").unwrap();
    assert!((v - 43.0).abs() < 0.001);
}

#[test]
fn plain_numeric_no_precision() {
    let eng = engine();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val NUMERIC)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id, val) VALUES (1, 3.125)", &[])
        .unwrap();
    let r = eng.sql("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    let v = float_col(&r.rows[0], "val").unwrap();
    assert!((v - 3.125).abs() < 0.001);
}

// =====================================================================
// STDDEV / VARIANCE
// =====================================================================

#[test]
fn stddev_samp() {
    let eng = engine_with_table();
    let r = eng.sql("SELECT stddev(val) AS v FROM t", &[]).unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    assert!((v - 10.0).abs() < 0.001);
}

#[test]
fn stddev_pop() {
    let eng = engine_with_table();
    let r = eng.sql("SELECT stddev_pop(val) AS v FROM t", &[]).unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    let expected = (200.0_f64 / 3.0).sqrt();
    assert!((v - expected).abs() < 0.001);
}

#[test]
fn stddev_single_row_is_null() {
    let eng = engine_with_table();
    let r = eng
        .sql("SELECT stddev(val) AS v FROM t WHERE id = 1", &[])
        .unwrap();
    assert!(matches!(r.rows[0].get("v"), Some(Value::Null) | None));
}

#[test]
fn variance_samp() {
    let eng = engine_with_table();
    let r = eng.sql("SELECT variance(val) AS v FROM t", &[]).unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    assert!((v - 100.0).abs() < 0.001);
}

#[test]
fn variance_pop() {
    let eng = engine_with_table();
    let r = eng.sql("SELECT var_pop(val) AS v FROM t", &[]).unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    let expected = 200.0 / 3.0;
    assert!((v - expected).abs() < 0.001);
}

// =====================================================================
// PERCENTILE_CONT / PERCENTILE_DISC
// =====================================================================

#[test]
fn percentile_cont_median() {
    let eng = engine_with_table();
    let r = eng
        .sql(
            "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY val) AS v FROM t",
            &[],
        )
        .unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    assert!((v - 20.0).abs() < 0.001);
}

#[test]
fn percentile_cont_quartile() {
    let eng = engine_with_table();
    let r = eng
        .sql(
            "SELECT percentile_cont(0.25) WITHIN GROUP (ORDER BY val) AS v FROM t",
            &[],
        )
        .unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    assert!((v - 15.0).abs() < 0.001);
}

#[test]
fn percentile_disc_median() {
    let eng = engine_with_table();
    let r = eng
        .sql(
            "SELECT percentile_disc(0.5) WITHIN GROUP (ORDER BY val) AS v FROM t",
            &[],
        )
        .unwrap();
    assert_eq!(int_col(&r.rows[0], "v"), Some(20));
}

// =====================================================================
// MODE
// =====================================================================

#[test]
fn mode_basic() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE m (id BIGSERIAL PRIMARY KEY, val INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO m (val) VALUES (1), (2), (2), (3)", &[])
        .unwrap();
    let r = eng
        .sql("SELECT mode() WITHIN GROUP (ORDER BY val) AS v FROM m", &[])
        .unwrap();
    assert_eq!(int_col(&r.rows[0], "v"), Some(2));
}
