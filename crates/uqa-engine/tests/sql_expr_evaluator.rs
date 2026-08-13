//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL expression-evaluator integration coverage: IS NULL / IS NOT NULL,
//! arithmetic, string concat, CASE / WHEN, CAST, COALESCE, string and
//! math scalar functions, expression-based WHERE clauses, mixed
//! projections, and IS NULL interactions with physical operators.

use uqa_core::Value;
use uqa_engine::Engine;

fn engine() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE products ( \
            id INTEGER PRIMARY KEY, \
            name TEXT NOT NULL, \
            price REAL, \
            quantity INTEGER, \
            category TEXT \
        )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO products (id, name, price, quantity, category) VALUES \
         (1, 'Widget', 10.50, 100, 'tools'), \
         (2, 'Gadget', 25.00, 50, 'electronics'), \
         (3, 'Doohickey', 5.75, 200, NULL)",
        &[],
    )
    .unwrap();
    eng
}

fn rows(eng: &Engine, sql: &str) -> Vec<uqa_sql::ResultRow> {
    eng.sql(sql, &[]).unwrap().rows
}

fn str_col<'a>(row: &'a uqa_sql::ResultRow, col: &str) -> Option<&'a str> {
    match row.get(col)? {
        Value::Str(s) => Some(s.as_str()),
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

fn int_col(row: &uqa_sql::ResultRow, col: &str) -> Option<i64> {
    match row.get(col)? {
        Value::Int(n) => Some(*n),
        _ => None,
    }
}

fn is_null(row: &uqa_sql::ResultRow, col: &str) -> bool {
    matches!(row.get(col), Some(Value::Null) | None)
}

// =====================================================================
// IS NULL / IS NOT NULL
// =====================================================================

#[test]
fn is_null_filters_null_category() {
    let eng = engine();
    let r = rows(&eng, "SELECT id, name FROM products WHERE category IS NULL");
    assert_eq!(r.len(), 1);
    assert_eq!(str_col(&r[0], "name"), Some("Doohickey"));
}

#[test]
fn is_not_null_filters_non_null_categories() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT id, name FROM products WHERE category IS NOT NULL",
    );
    assert_eq!(r.len(), 2);
    let names: std::collections::BTreeSet<&str> =
        r.iter().filter_map(|row| str_col(row, "name")).collect();
    assert!(names.contains("Widget"));
    assert!(names.contains("Gadget"));
}

#[test]
fn is_null_combines_with_and() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name FROM products WHERE category IS NOT NULL AND price > 15",
    );
    assert_eq!(r.len(), 1);
    assert_eq!(str_col(&r[0], "name"), Some("Gadget"));
}

#[test]
fn is_null_on_non_null_column_yields_zero() {
    let eng = engine();
    let r = rows(&eng, "SELECT id FROM products WHERE name IS NULL");
    assert!(r.is_empty());
}

#[test]
fn is_not_null_returns_all_rows() {
    let eng = engine();
    let r = rows(&eng, "SELECT id FROM products WHERE price IS NOT NULL");
    assert_eq!(r.len(), 3);
}

// =====================================================================
// Arithmetic expressions in SELECT
// =====================================================================

#[test]
fn standalone_literal_select_projects_value() {
    let eng = Engine::new();
    let r = eng.sql("SELECT 1", &[]).unwrap();
    assert_eq!(r.columns, vec!["?column?"]);
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0].get("?column?"), Some(&Value::Int(1)));
}

#[test]
fn arithmetic_multiply() {
    let eng = engine();
    let r = rows(&eng, "SELECT name, price * 2 AS double_price FROM products");
    assert_eq!(r.len(), 3);
    assert_eq!(float_col(&r[0], "double_price"), Some(21.0));
}

#[test]
fn arithmetic_add() {
    let eng = engine();
    let r = rows(&eng, "SELECT name, price + 1 AS incremented FROM products");
    assert_eq!(float_col(&r[0], "incremented"), Some(11.5));
}

#[test]
fn arithmetic_subtract() {
    let eng = engine();
    let r = rows(&eng, "SELECT name, price - 5 AS discounted FROM products");
    assert_eq!(float_col(&r[0], "discounted"), Some(5.5));
}

#[test]
fn arithmetic_unary_plus_and_minus() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT -quantity AS neg_quantity, +quantity AS pos_quantity FROM products WHERE id = 1",
    );
    assert_eq!(int_col(&r[0], "neg_quantity"), Some(-100));
    assert_eq!(int_col(&r[0], "pos_quantity"), Some(100));
}

#[test]
fn arithmetic_divide() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name, price / quantity AS unit_cost FROM products",
    );
    let v = float_col(&r[0], "unit_cost").unwrap();
    assert!((v - 0.105).abs() < 0.001);
}

#[test]
fn arithmetic_modulo() {
    let eng = engine();
    let r = rows(&eng, "SELECT id, quantity % 60 AS remainder FROM products");
    assert_eq!(int_col(&r[0], "remainder"), Some(40));
    assert_eq!(int_col(&r[1], "remainder"), Some(50));
}

#[test]
fn arithmetic_integer_division() {
    let eng = engine();
    let r = rows(&eng, "SELECT id, quantity / 3 AS thirds FROM products");
    assert_eq!(int_col(&r[0], "thirds"), Some(33));
}

#[test]
fn arithmetic_with_null_propagates_null() {
    let eng = engine();
    eng.sql(
        "INSERT INTO products (id, name, price, quantity) \
         VALUES (4, 'NullItem', NULL, 10)",
        &[],
    )
    .unwrap();
    let r = rows(
        &eng,
        "SELECT name, price * 2 AS dp FROM products WHERE id = 4",
    );
    assert!(is_null(&r[0], "dp"));
}

#[test]
fn arithmetic_compound_expression() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name, (price * quantity) + 10 AS total FROM products",
    );
    let v = float_col(&r[0], "total").unwrap();
    assert!((v - 1060.0).abs() < 0.01);
}

#[test]
fn arithmetic_division_by_zero_errors() {
    // PostgreSQL 18 raises `division by zero` (SQLSTATE 22012) instead
    // of yielding NULL.
    let eng = engine();
    let err = eng
        .sql("SELECT name, price / 0 AS bad FROM products LIMIT 1", &[])
        .unwrap_err();
    assert!(err.to_string().contains("division by zero"));
}

// =====================================================================
// String concatenation
// =====================================================================

#[test]
fn string_concat_basic() {
    let eng = engine();
    let r = rows(&eng, "SELECT name || '!' AS excited FROM products");
    assert_eq!(str_col(&r[0], "excited"), Some("Widget!"));
}

#[test]
fn string_concat_multi_with_cast() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name || ' ($' || CAST(price AS TEXT) || ')' AS label FROM products",
    );
    assert_eq!(str_col(&r[0], "label"), Some("Widget ($10.5)"));
}

#[test]
fn string_concat_with_null_propagates_null() {
    let eng = engine();
    eng.sql(
        "INSERT INTO products (id, name, price, quantity, category) \
         VALUES (4, 'NullCat', 1.0, 1, NULL)",
        &[],
    )
    .unwrap();
    let r = rows(
        &eng,
        "SELECT name || category AS result FROM products WHERE id = 4",
    );
    assert!(is_null(&r[0], "result"));
}

// =====================================================================
// CASE / WHEN
// =====================================================================

#[test]
fn case_simple() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name, CASE WHEN price > 20 THEN 'expensive' ELSE 'affordable' END AS tier FROM products",
    );
    assert_eq!(str_col(&r[0], "tier"), Some("affordable"));
    assert_eq!(str_col(&r[1], "tier"), Some("expensive"));
}

#[test]
fn case_multi_when() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name, CASE WHEN price > 20 THEN 'high' \
         WHEN price > 8 THEN 'medium' ELSE 'low' END AS tier FROM products",
    );
    assert_eq!(str_col(&r[0], "tier"), Some("medium"));
    assert_eq!(str_col(&r[1], "tier"), Some("high"));
    assert_eq!(str_col(&r[2], "tier"), Some("low"));
}

#[test]
fn case_no_else_yields_null() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name, CASE WHEN price > 20 THEN 'expensive' END AS tier FROM products",
    );
    assert!(is_null(&r[0], "tier"));
    assert_eq!(str_col(&r[1], "tier"), Some("expensive"));
}

#[test]
fn case_with_null_branch() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name, CASE WHEN category IS NULL THEN 'uncategorized' \
         ELSE category END AS cat FROM products",
    );
    assert_eq!(str_col(&r[2], "cat"), Some("uncategorized"));
}

// =====================================================================
// CAST
// =====================================================================

#[test]
fn cast_int_to_text() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT CAST(quantity AS TEXT) AS qty_text FROM products",
    );
    assert_eq!(str_col(&r[0], "qty_text"), Some("100"));
}

#[test]
fn cast_text_to_int() {
    let eng = engine();
    eng.sql("CREATE TABLE nums (id INTEGER, val TEXT)", &[])
        .unwrap();
    eng.sql("INSERT INTO nums (id, val) VALUES (1, '42')", &[])
        .unwrap();
    let r = rows(&eng, "SELECT CAST(val AS INTEGER) AS num FROM nums");
    assert_eq!(int_col(&r[0], "num"), Some(42));
}

#[test]
fn cast_float_to_int() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT CAST(price AS INTEGER) AS price_int FROM products",
    );
    assert_eq!(int_col(&r[0], "price_int"), Some(10));
}

#[test]
fn cast_null_stays_null() {
    let eng = engine();
    eng.sql(
        "INSERT INTO products (id, name, price, quantity) \
         VALUES (4, 'NullItem', NULL, 1)",
        &[],
    )
    .unwrap();
    let r = rows(
        &eng,
        "SELECT CAST(price AS TEXT) AS p FROM products WHERE id = 4",
    );
    assert!(is_null(&r[0], "p"));
}

// =====================================================================
// COALESCE
// =====================================================================

#[test]
fn coalesce_basic() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT id, COALESCE(category, 'none') AS cat FROM products",
    );
    assert_eq!(str_col(&r[0], "cat"), Some("tools"));
    assert_eq!(str_col(&r[2], "cat"), Some("none"));
}

#[test]
fn coalesce_first_non_null() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT COALESCE(NULL, NULL, 'fallback') AS val FROM products LIMIT 1",
    );
    assert_eq!(str_col(&r[0], "val"), Some("fallback"));
}

#[test]
fn coalesce_all_non_null() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT COALESCE(name, 'default') AS val FROM products LIMIT 1",
    );
    assert_eq!(str_col(&r[0], "val"), Some("Widget"));
}

// =====================================================================
// String functions
// =====================================================================

#[test]
fn string_upper() {
    let eng = engine();
    let r = rows(&eng, "SELECT UPPER(name) AS up FROM products");
    assert_eq!(str_col(&r[0], "up"), Some("WIDGET"));
}

#[test]
fn string_lower() {
    let eng = engine();
    let r = rows(&eng, "SELECT LOWER(name) AS low FROM products");
    assert_eq!(str_col(&r[0], "low"), Some("widget"));
}

#[test]
fn string_length() {
    let eng = engine();
    let r = rows(&eng, "SELECT LENGTH(name) AS len FROM products");
    assert_eq!(int_col(&r[0], "len"), Some(6));
}

#[test]
fn string_substring() {
    let eng = engine();
    let r = rows(&eng, "SELECT SUBSTRING(name, 1, 3) AS prefix FROM products");
    assert_eq!(str_col(&r[0], "prefix"), Some("Wid"));
}

#[test]
fn string_replace() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT REPLACE(name, 'dget', 'DGET') AS replaced FROM products",
    );
    assert_eq!(str_col(&r[0], "replaced"), Some("WiDGET"));
    assert_eq!(str_col(&r[1], "replaced"), Some("GaDGET"));
}

#[test]
fn string_trim() {
    let eng = engine();
    eng.sql("CREATE TABLE ws (id INTEGER, val TEXT)", &[])
        .unwrap();
    eng.sql("INSERT INTO ws (id, val) VALUES (1, '  hello  ')", &[])
        .unwrap();
    let r = rows(&eng, "SELECT TRIM(val) AS trimmed FROM ws");
    assert_eq!(str_col(&r[0], "trimmed"), Some("hello"));
}

#[test]
fn string_concat_function() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT CONCAT(name, ' - ', category) AS label FROM products",
    );
    assert_eq!(str_col(&r[0], "label"), Some("Widget - tools"));
    // NULL category becomes empty string in CONCAT()
    assert_eq!(str_col(&r[2], "label"), Some("Doohickey - "));
}

#[test]
fn string_left_prefix() {
    let eng = engine();
    let r = rows(&eng, "SELECT LEFT(name, 3) AS prefix FROM products");
    assert_eq!(str_col(&r[0], "prefix"), Some("Wid"));
}

#[test]
fn string_right_suffix() {
    let eng = engine();
    let r = rows(&eng, "SELECT RIGHT(name, 3) AS suffix FROM products");
    assert_eq!(str_col(&r[0], "suffix"), Some("get"));
}

#[test]
fn string_function_on_null_propagates_null() {
    let eng = engine();
    eng.sql(
        "INSERT INTO products (id, name, price, quantity, category) \
         VALUES (4, 'X', 1.0, 1, NULL)",
        &[],
    )
    .unwrap();
    let r = rows(
        &eng,
        "SELECT UPPER(category) AS up FROM products WHERE id = 4",
    );
    assert!(is_null(&r[0], "up"));
}

// =====================================================================
// Math functions
// =====================================================================

#[test]
fn math_abs() {
    let eng = engine();
    let r = rows(&eng, "SELECT ABS(price - 10) AS diff FROM products");
    let v = float_col(&r[0], "diff").unwrap();
    assert!((v - 0.5).abs() < 0.01);
}

#[test]
fn math_round_with_decimals() {
    let eng = engine();
    let r = rows(&eng, "SELECT ROUND(price, 1) AS rounded FROM products");
    assert_eq!(float_col(&r[0], "rounded"), Some(10.5));
}

#[test]
fn math_round_no_decimals() {
    let eng = engine();
    let r = rows(&eng, "SELECT ROUND(price) AS rounded FROM products");
    let v = match r[0].get("rounded").unwrap() {
        Value::Int(n) => *n as f64,
        Value::Float(f) => *f,
        other => panic!("unexpected: {other:?}"),
    };
    assert!((10.0 - v).abs() < 1.0 || (11.0 - v).abs() < 1.0);
}

#[test]
fn math_ceil() {
    let eng = engine();
    let r = rows(&eng, "SELECT CEIL(price) AS c FROM products");
    let v = match r[0].get("c").unwrap() {
        Value::Int(n) => *n,
        Value::Float(f) => *f as i64,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(v, 11);
}

#[test]
fn math_floor() {
    let eng = engine();
    let r = rows(&eng, "SELECT FLOOR(price) AS f FROM products");
    let v = match r[0].get("f").unwrap() {
        Value::Int(n) => *n,
        Value::Float(f) => *f as i64,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(v, 10);
}

// =====================================================================
// Expression-based WHERE clause
// =====================================================================

#[test]
fn where_arithmetic_comparison() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name FROM products WHERE price * quantity > 1100",
    );
    let names: std::collections::BTreeSet<String> = r
        .iter()
        .filter_map(|row| str_col(row, "name").map(str::to_string))
        .collect();
    assert!(names.contains("Gadget"));
    assert!(names.contains("Doohickey"));
    assert_eq!(r.len(), 2);
}

#[test]
fn where_expression_left_side() {
    let eng = engine();
    let r = rows(&eng, "SELECT name FROM products WHERE price * 2 > 15");
    let names: std::collections::BTreeSet<String> = r
        .iter()
        .filter_map(|row| str_col(row, "name").map(str::to_string))
        .collect();
    assert!(names.contains("Widget"));
    assert!(names.contains("Gadget"));
}

#[test]
fn where_combined_expression_and_column() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name FROM products WHERE quantity >= 100 AND price * 2 > 15",
    );
    assert_eq!(r.len(), 1);
    assert_eq!(str_col(&r[0], "name"), Some("Widget"));
}

#[test]
fn where_expression_no_match() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name FROM products WHERE price * quantity > 99999",
    );
    assert!(r.is_empty());
}

// =====================================================================
// Mixed: computed expressions with simple columns
// =====================================================================

#[test]
fn mixed_simple_and_computed() {
    let eng = engine();
    let r = eng
        .sql(
            "SELECT id, name, price * quantity AS total FROM products",
            &[],
        )
        .unwrap();
    assert_eq!(r.columns, vec!["id", "name", "total"]);
    assert_eq!(int_col(&r.rows[0], "id"), Some(1));
    assert_eq!(str_col(&r.rows[0], "name"), Some("Widget"));
    let v = float_col(&r.rows[0], "total").unwrap();
    assert!((v - 1050.0).abs() < 0.01);
}

#[test]
fn mixed_all_computed() {
    let eng = engine();
    let r = eng
        .sql(
            "SELECT price * 2 AS dp, quantity + 10 AS q10 FROM products",
            &[],
        )
        .unwrap();
    assert_eq!(r.columns, vec!["dp", "q10"]);
    assert_eq!(float_col(&r.rows[0], "dp"), Some(21.0));
    assert_eq!(int_col(&r.rows[0], "q10"), Some(110));
}

#[test]
fn mixed_computed_with_order_by() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name, price * quantity AS total FROM products ORDER BY total DESC",
    );
    assert_eq!(str_col(&r[0], "name"), Some("Gadget"));
    assert_eq!(str_col(&r[2], "name"), Some("Widget"));
}

#[test]
fn order_by_output_alias_wins_over_same_named_input_column() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name, quantity AS price FROM products ORDER BY price ASC",
    );
    assert_eq!(str_col(&r[0], "name"), Some("Gadget"));
    assert_eq!(str_col(&r[2], "name"), Some("Doohickey"));
}

#[test]
fn order_by_preserves_repeated_source_column_under_alias() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name, name AS name_copy FROM products ORDER BY id",
    );
    assert_eq!(str_col(&r[0], "name"), Some("Widget"));
    assert_eq!(str_col(&r[0], "name_copy"), Some("Widget"));
}

#[test]
fn order_by_can_use_ordinal_or_unselected_input_column() {
    let eng = engine();
    let ordinal = rows(&eng, "SELECT name, quantity FROM products ORDER BY 2 DESC");
    assert_eq!(str_col(&ordinal[0], "name"), Some("Doohickey"));

    let unselected = rows(&eng, "SELECT name FROM products ORDER BY quantity DESC");
    assert_eq!(str_col(&unselected[0], "name"), Some("Doohickey"));
    assert_eq!(unselected[0].len(), 1);
}

#[test]
fn order_by_rejects_an_out_of_range_ordinal() {
    let eng = engine();
    let error = eng
        .sql("SELECT name FROM products ORDER BY 0", &[])
        .unwrap_err();
    assert!(error.to_string().contains("ORDER BY position 0"), "{error}");
    let error = eng
        .sql("SELECT name FROM products ORDER BY 2", &[])
        .unwrap_err();
    assert!(error.to_string().contains("ORDER BY position 2"), "{error}");
}

#[test]
fn order_by_alias_does_not_re_evaluate_volatile_projection() {
    let eng = engine();
    eng.sql("CREATE SEQUENCE order_eval_seq START 1", &[])
        .unwrap();
    let r = rows(
        &eng,
        "SELECT nextval('order_eval_seq') AS n FROM products ORDER BY n DESC",
    );
    assert_eq!(int_col(&r[0], "n"), Some(3));
    assert_eq!(int_col(&r[2], "n"), Some(1));
    assert_eq!(eng.currval("order_eval_seq").unwrap(), 3);
}

#[test]
fn mixed_computed_with_limit() {
    let eng = engine();
    let r = rows(&eng, "SELECT name, price * 2 AS dp FROM products LIMIT 2");
    assert_eq!(r.len(), 2);
}

// =====================================================================
// IS NULL with physical operators
// =====================================================================

#[test]
fn null_with_group_by() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT category, COUNT(*) AS cnt FROM products GROUP BY category",
    );
    let mut by_category: std::collections::BTreeMap<Option<String>, i64> =
        std::collections::BTreeMap::new();
    for row in &r {
        let category = match row.get("category") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        };
        let count = int_col(row, "cnt").unwrap_or(0);
        by_category.insert(category, count);
    }
    assert_eq!(by_category.get(&None).copied(), Some(1));
    assert_eq!(
        by_category.get(&Some("tools".to_string())).copied(),
        Some(1)
    );
    assert_eq!(
        by_category.get(&Some("electronics".to_string())).copied(),
        Some(1)
    );
}

#[test]
fn null_with_order_by() {
    let eng = engine();
    let r = rows(
        &eng,
        "SELECT name FROM products WHERE category IS NOT NULL ORDER BY name",
    );
    assert_eq!(str_col(&r[0], "name"), Some("Gadget"));
    assert_eq!(str_col(&r[1], "name"), Some("Widget"));
}

#[test]
fn null_with_distinct() {
    let eng = engine();
    eng.sql(
        "INSERT INTO products (id, name, price, quantity, category) \
         VALUES (4, 'Thingamajig', 3.00, 10, NULL)",
        &[],
    )
    .unwrap();
    let r = rows(
        &eng,
        "SELECT DISTINCT category FROM products WHERE category IS NOT NULL",
    );
    let cats: std::collections::BTreeSet<String> = r
        .iter()
        .filter_map(|row| str_col(row, "category").map(str::to_string))
        .collect();
    assert!(cats.contains("tools"));
    assert!(cats.contains("electronics"));
}

#[test]
fn distinct_on_keeps_first_ordered_row_per_key() {
    let eng = engine();
    eng.sql(
        "INSERT INTO products (id, name, price, quantity, category) \
         VALUES (4, 'Toolbox', 20.00, 8, 'tools')",
        &[],
    )
    .unwrap();

    let r = rows(
        &eng,
        "SELECT DISTINCT ON (category) category, name \
         FROM products \
         WHERE category IS NOT NULL \
         ORDER BY category, price DESC",
    );

    assert_eq!(r.len(), 2);
    assert_eq!(str_col(&r[0], "category"), Some("electronics"));
    assert_eq!(str_col(&r[0], "name"), Some("Gadget"));
    assert_eq!(str_col(&r[1], "category"), Some("tools"));
    assert_eq!(str_col(&r[1], "name"), Some("Toolbox"));
}

#[test]
fn distinct_on_applies_limit_after_dedup() {
    let eng = engine();
    eng.sql(
        "INSERT INTO products (id, name, price, quantity, category) VALUES \
         (4, 'Cable', 3.00, 30, 'electronics'), \
         (5, 'Toolbox', 20.00, 8, 'tools')",
        &[],
    )
    .unwrap();

    let r = rows(
        &eng,
        "SELECT DISTINCT ON (category) category, name \
         FROM products \
         WHERE category IS NOT NULL \
         ORDER BY category, price DESC \
         LIMIT 2",
    );

    assert_eq!(r.len(), 2);
    assert_eq!(str_col(&r[0], "category"), Some("electronics"));
    assert_eq!(str_col(&r[0], "name"), Some("Gadget"));
    assert_eq!(str_col(&r[1], "category"), Some("tools"));
    assert_eq!(str_col(&r[1], "name"), Some("Toolbox"));
}

#[test]
fn relational_filter_errors_are_not_empty_results() {
    let eng = engine();
    let error = eng
        .sql(
            "SELECT id FROM products WHERE quantity / (quantity - quantity) > 0",
            &[],
        )
        .unwrap_err();
    assert!(error.to_string().contains("division by zero"));
}

#[test]
fn hash_join_key_errors_propagate() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE lhs (id INTEGER)", &[]).unwrap();
    eng.sql("CREATE TABLE rhs (id INTEGER)", &[]).unwrap();
    eng.sql("INSERT INTO lhs (id) VALUES (1), (2)", &[])
        .unwrap();
    eng.sql("INSERT INTO rhs (id) VALUES (1), (2)", &[])
        .unwrap();

    // The first row selects the hash-join access path; the second row then
    // fails while computing that same physical key.
    let error = eng
        .sql(
            "SELECT lhs.id FROM lhs JOIN rhs ON lhs.id / (2 - lhs.id) = rhs.id",
            &[],
        )
        .unwrap_err();
    assert!(error.to_string().contains("division by zero"));
}

#[test]
fn aggregate_filter_errors_propagate() {
    let eng = engine();
    let error = eng
        .sql(
            "SELECT SUM(quantity) FILTER (WHERE quantity / (quantity - quantity) > 0) \
             FROM products",
            &[],
        )
        .unwrap_err();
    assert!(error.to_string().contains("division by zero"));
}
