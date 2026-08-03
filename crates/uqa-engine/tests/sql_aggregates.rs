//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL aggregate coverage, including `COUNT DISTINCT`,
//! `STRING_AGG` / `ARRAY_AGG` with `DISTINCT` and `ORDER BY`, `BOOL_AND` /
//! `BOOL_OR`, `FILTER (WHERE ...)`, `GROUP BY` by ordinal / alias,
//! complex `HAVING`, `NUMERIC` precision/scale, `STDDEV` / `VARIANCE`,
//! `PERCENTILE_CONT` / `PERCENTILE_DISC`, and `MODE`.

use uqa_core::{DecimalValue, Value};
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
        // Statistical aggregates over integer columns return numeric
        // (PostgreSQL 17), so accept exact decimals here too.
        Value::Decimal(d) => d.to_f64(),
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

fn dec(value: &str) -> Value {
    Value::Decimal(DecimalValue::parse(value).unwrap())
}

fn decimal_col(row: &uqa_sql::ResultRow, col: &str) -> Option<Value> {
    match row.get(col)? {
        Value::Decimal(value) => Some(Value::Decimal(value.clone())),
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
fn qualified_columns_work_in_projected_single_table_aggregates() {
    let eng = engine_with_data();
    let r = eng
        .sql(
            "SELECT SUM(u.age) AS total, AVG(u.age) AS mean, COUNT(*) AS cnt FROM users AS u",
            &[],
        )
        .unwrap();

    assert_eq!(int_col(&r.rows[0], "total"), Some(115));
    assert_eq!(float_col(&r.rows[0], "mean"), Some(28.75));
    assert_eq!(int_col(&r.rows[0], "cnt"), Some(4));
}

#[test]
fn scalar_functions_inside_aggregates_use_the_materialized_fallback() {
    let eng = engine_with_data();
    let r = eng
        .sql("SELECT SUM(ABS(age)) AS total FROM users", &[])
        .unwrap();

    assert_eq!(int_col(&r.rows[0], "total"), Some(115));
}

#[test]
fn projected_group_cache_falls_back_for_high_cardinality_keys() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE group_cardinality (id INTEGER PRIMARY KEY, amount INTEGER)",
        &[],
    )
    .unwrap();
    let values = (0..40)
        .map(|id| format!("({id}, {})", id * 2))
        .collect::<Vec<_>>()
        .join(", ");
    eng.sql(
        &format!("INSERT INTO group_cardinality (id, amount) VALUES {values}"),
        &[],
    )
    .unwrap();

    let r = eng
        .sql(
            "SELECT id, SUM(amount) AS total, COUNT(*) AS cnt \
             FROM group_cardinality GROUP BY id ORDER BY id",
            &[],
        )
        .unwrap();

    assert_eq!(r.rows.len(), 40);
    assert_eq!(int_col(&r.rows[39], "id"), Some(39));
    assert_eq!(int_col(&r.rows[39], "total"), Some(78));
    assert_eq!(int_col(&r.rows[39], "cnt"), Some(1));
}

#[test]
fn projected_integer_arithmetic_preserves_null_and_error_semantics() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE integer_arithmetic (id INTEGER PRIMARY KEY, lhs INTEGER, rhs INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO integer_arithmetic (id, lhs, rhs) VALUES \
         (1, 10, 2), (2, 7, 3), (3, NULL, 4)",
        &[],
    )
    .unwrap();

    let result = eng
        .sql(
            "SELECT SUM(lhs * rhs + 1) AS total, AVG(lhs - rhs) AS mean \
             FROM integer_arithmetic",
            &[],
        )
        .unwrap();
    assert_eq!(int_col(&result.rows[0], "total"), Some(43));
    assert_eq!(float_col(&result.rows[0], "mean"), Some(6.0));

    eng.sql(
        "INSERT INTO integer_arithmetic (id, lhs, rhs) VALUES (4, 1, 0)",
        &[],
    )
    .unwrap();
    let error = eng
        .sql("SELECT SUM(lhs / rhs) FROM integer_arithmetic", &[])
        .unwrap_err();
    assert!(error.to_string().contains("division by zero"));
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
fn numeric_distinct_collapses_decimal_scale() {
    let eng = engine();
    eng.sql("CREATE TABLE amounts (amount NUMERIC)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO amounts (amount) VALUES (1.0), (1.00), (2.0)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT COUNT(DISTINCT amount) AS cnt, SUM(DISTINCT amount) AS total FROM amounts",
            &[],
        )
        .unwrap();
    assert_eq!(int_col(&r.rows[0], "cnt"), Some(2));
    assert_eq!(decimal_col(&r.rows[0], "total"), Some(dec("3.0")));
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

#[test]
fn aggregate_type_errors_are_not_silently_ignored() {
    let eng = engine_with_table();
    let sum_error = eng
        .sql("SELECT SUM(name) FROM t", &[])
        .expect_err("SUM over text must fail");
    assert!(sum_error.to_string().contains("numeric"), "{sum_error}");

    let bool_error = eng
        .sql("SELECT BOOL_AND(val) FROM t", &[])
        .expect_err("BOOL_AND over integers must fail");
    assert!(bool_error.to_string().contains("boolean"), "{bool_error}");
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

#[test]
fn group_by_alias_for_cast_expression() {
    let eng = engine();
    eng.sql("CREATE TABLE points (id INTEGER PRIMARY KEY, x REAL)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO points (id, x) VALUES (1, 1.2), (2, 1.8), (3, 2.1)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT CAST(x AS INT) AS tile_x, COUNT(*) AS cnt
             FROM points
             GROUP BY tile_x
             ORDER BY tile_x",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 2);
    // PostgreSQL 17: float8 -> int casts round half to even, so
    // 1.2 -> 1 and 1.8 / 2.1 -> 2 (verified: CAST(1.8::float8 AS int) = 2).
    assert_eq!(r.rows[0]["tile_x"], Value::Int(1));
    assert_eq!(r.rows[0]["cnt"], Value::Int(1));
    assert_eq!(r.rows[1]["tile_x"], Value::Int(2));
    assert_eq!(r.rows[1]["cnt"], Value::Int(2));
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

#[test]
fn having_count_equals_grouped_column() {
    // Regression: HAVING comparing an aggregate to a grouped column that is not itself
    // projected must filter by the per-group column value, not silently drop every row.
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, cat TEXT, need BIGINT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO t (id, cat, need) VALUES (1, 'a', 2), (2, 'a', 2), (3, 'b', 5)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT cat, COUNT(*) AS cnt FROM t GROUP BY cat, need HAVING COUNT(*) = need",
            &[],
        )
        .unwrap();
    // Group 'a' has count 2 == need 2; group 'b' has count 1 != need 5.
    assert_eq!(r.rows.len(), 1);
    assert_eq!(str_col(&r.rows[0], "cat"), Some("a"));
}

#[path = "sql_aggregates/numeric_statistics.rs"]
mod numeric_statistics;
