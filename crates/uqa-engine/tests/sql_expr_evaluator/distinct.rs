//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
fn distinct_on_keeps_an_unprojected_key_until_deduplication() {
    let eng = Engine::new();
    let result = eng
        .sql(
            "SELECT DISTINCT ON (x) y FROM (VALUES (2, 'b'), (1, 'c'), (1, 'a')) AS input(x, y) ORDER BY x, y",
            &[],
        )
        .unwrap();

    assert_eq!(result.columns, ["y"]);
    assert_eq!(result.value_at(0, 0), Some(&Value::Str("a".into())));
    assert_eq!(result.value_at(1, 0), Some(&Value::Str("b".into())));
}

#[test]
fn distinct_on_reuses_volatile_target_and_order_keys() {
    let eng = Engine::new();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let function_calls = std::sync::Arc::clone(&calls);
    eng.register_scalar_function_with_options(
        "distinct_key_call",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
        move |_args: &[Value]| {
            Ok(Value::Int(
                function_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1,
            ))
        },
    )
    .unwrap();

    let hidden = eng
        .sql(
            "SELECT DISTINCT ON (distinct_key_call()) 0 AS value FROM (VALUES (1), (2), (3)) AS input(id) ORDER BY distinct_key_call()",
            &[],
        )
        .unwrap();
    assert_eq!(hidden.rows.len(), 3);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);

    calls.store(0, std::sync::atomic::Ordering::SeqCst);
    let projected = eng
        .sql(
            "SELECT DISTINCT ON (distinct_key_call()) distinct_key_call() AS value FROM (VALUES (1), (2), (3)) AS input(id) ORDER BY distinct_key_call()",
            &[],
        )
        .unwrap();
    assert_eq!(
        projected
            .rows
            .iter()
            .map(|row| row["value"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2), Value::Int(3)]
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);

    calls.store(0, std::sync::atomic::Ordering::SeqCst);
    let duplicate_distinct = eng
        .sql(
            "SELECT DISTINCT ON (distinct_key_call(), distinct_key_call()) 0 AS value FROM (VALUES (1), (2), (3)) AS input(id) ORDER BY distinct_key_call()",
            &[],
        )
        .unwrap();
    assert_eq!(duplicate_distinct.rows.len(), 3);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);

    calls.store(0, std::sync::atomic::Ordering::SeqCst);
    let duplicate_order = eng
        .sql(
            "SELECT 0 AS value FROM (VALUES (1), (2), (3)) AS input(id) ORDER BY distinct_key_call(), distinct_key_call()",
            &[],
        )
        .unwrap();
    assert_eq!(duplicate_order.rows.len(), 3);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);

    calls.store(0, std::sync::atomic::Ordering::SeqCst);
    let grouped = eng
        .sql(
            "SELECT DISTINCT ON (distinct_key_call()) count(*) AS value FROM (VALUES (2), (1), (2)) AS input(id) GROUP BY id ORDER BY distinct_key_call()",
            &[],
        )
        .unwrap();
    assert_eq!(grouped.rows.len(), 2);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn distinct_ordering_validation_matches_postgresql() {
    let eng = Engine::new();
    let distinct_order = eng
        .sql(
            "SELECT DISTINCT y FROM (VALUES (2, 'b'), (1, 'a')) AS input(x, y) ORDER BY x",
            &[],
        )
        .unwrap_err();
    assert_eq!(distinct_order.sqlstate(), Some("42P10"));
    assert!(distinct_order
        .to_string()
        .contains("ORDER BY expressions must appear in select list"));

    let distinct_on_order = eng
        .sql(
            "SELECT DISTINCT ON (x) y FROM (VALUES (1, 'a'), (2, 'b')) AS input(x, y) ORDER BY y, x",
            &[],
        )
        .unwrap_err();
    assert_eq!(distinct_on_order.sqlstate(), Some("42P10"));
    assert!(distinct_on_order
        .to_string()
        .contains("DISTINCT ON expressions must match initial ORDER BY expressions"));

    let ordinal = eng
        .sql(
            "SELECT DISTINCT ON (1) x, y FROM (VALUES (1, 20), (1, 10), (2, 30)) AS input(x, y) ORDER BY 1, y",
            &[],
        )
        .unwrap();
    assert_eq!(ordinal.rows.len(), 2);
    assert_eq!(ordinal.value_at(0, 1), Some(&Value::Int(10)));
    assert_eq!(ordinal.value_at(1, 1), Some(&Value::Int(30)));

    let output_alias = eng
        .sql(
            "SELECT DISTINCT ON (output_key) x AS output_key, y FROM (VALUES (1, 20), (1, 10), (2, 30)) AS input(x, y) ORDER BY output_key, y",
            &[],
        )
        .unwrap();
    assert_eq!(output_alias.rows.len(), 2);
    assert_eq!(output_alias.value_at(0, 1), Some(&Value::Int(10)));
    assert_eq!(output_alias.value_at(1, 1), Some(&Value::Int(30)));

    let out_of_range = eng
        .sql(
            "SELECT DISTINCT ON (3) x, y FROM (VALUES (1, 10)) AS input(x, y) ORDER BY x, y",
            &[],
        )
        .unwrap_err();
    assert_eq!(out_of_range.sqlstate(), Some("42P10"));
    assert!(out_of_range
        .to_string()
        .contains("DISTINCT ON position 3 is not in the select list"));

    let reordered = eng
        .sql(
            "SELECT DISTINCT ON (x, y) x, y FROM (VALUES (1, 1), (1, 2)) AS input(x, y) ORDER BY y, x",
            &[],
        )
        .unwrap();
    assert_eq!(reordered.rows.len(), 2);
    let prefix_subset = eng
        .sql(
            "SELECT DISTINCT ON (x, y) x, y FROM (VALUES (1, 1), (1, 2)) AS input(x, y) ORDER BY x",
            &[],
        )
        .unwrap();
    assert_eq!(prefix_subset.rows.len(), 2);
    let duplicate_key = eng
        .sql(
            "SELECT DISTINCT ON (x, x) x FROM (VALUES (1), (2)) AS input(x) ORDER BY x, x + 0",
            &[],
        )
        .unwrap();
    assert_eq!(duplicate_key.rows.len(), 2);
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
