//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! GROUPING SETS / ROLLUP / CUBE expansion.

use uqa_core::Value;
use uqa_engine::Engine;

fn setup() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE sales (id INTEGER PRIMARY KEY, region TEXT, product TEXT, amount INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO sales (id, region, product, amount) VALUES \
         (1, 'us', 'a', 10), (2, 'us', 'b', 20), \
         (3, 'eu', 'a', 30), (4, 'eu', 'b', 40)",
        &[],
    )
    .unwrap();
    eng
}

#[test]
fn rollup_emits_subtotals_and_grand_total() {
    let eng = setup();
    let r = eng
        .sql(
            "SELECT region, sum(amount) AS total FROM sales GROUP BY ROLLUP(region)",
            &[],
        )
        .unwrap();
    // ROLLUP(region) -> (region), () = 2 grouping sets.
    // Per region: us=30, eu=70; grand total: 100.
    let totals: Vec<i64> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("total") {
            Some(Value::Int(n)) => Some(*n),
            Some(Value::Float(f)) => Some(*f as i64),
            _ => None,
        })
        .collect();
    assert!(totals.contains(&30));
    assert!(totals.contains(&70));
    assert!(totals.contains(&100));
}

#[test]
fn cube_emits_all_subtotals() {
    let eng = setup();
    let r = eng
        .sql(
            "SELECT region, product, sum(amount) AS total FROM sales \
             GROUP BY CUBE(region, product)",
            &[],
        )
        .unwrap();
    // CUBE(region, product) -> (), (region), (product), (region, product) = 4 sets.
    // 4 rows for full set, 2 for region only, 2 for product only, 1 grand total = 9 rows.
    assert_eq!(r.rows.len(), 9);
    let totals: Vec<i64> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("total") {
            Some(Value::Int(n)) => Some(*n),
            Some(Value::Float(f)) => Some(*f as i64),
            _ => None,
        })
        .collect();
    assert!(totals.contains(&100));
}

#[test]
fn group_by_distinct_removes_only_duplicate_grouping_sets() {
    let eng = setup();
    let distinct = eng
        .sql(
            "SELECT region, sum(amount) AS total FROM sales \
             GROUP BY DISTINCT GROUPING SETS ((region), (region), ()) \
             ORDER BY region NULLS LAST, total",
            &[],
        )
        .unwrap();
    assert_eq!(distinct.rows.len(), 3);

    let all = eng
        .sql(
            "SELECT region, sum(amount) AS total FROM sales \
             GROUP BY ALL GROUPING SETS ((region), (region), ()) \
             ORDER BY region NULLS LAST, total",
            &[],
        )
        .unwrap();
    assert_eq!(all.rows.len(), 5);

    let reordered = eng
        .sql(
            "SELECT region, product, count(*) AS n FROM sales \
             GROUP BY DISTINCT GROUPING SETS ((region, product), (product, region))",
            &[],
        )
        .unwrap();
    assert_eq!(reordered.rows.len(), 4);

    let rollup = eng
        .sql(
            "SELECT region, count(*) AS n FROM sales \
             GROUP BY DISTINCT ROLLUP(region, region)",
            &[],
        )
        .unwrap();
    assert_eq!(rollup.rows.len(), 3);

    let cube = eng
        .sql(
            "SELECT region, count(*) AS n FROM sales \
             GROUP BY DISTINCT CUBE(region, region)",
            &[],
        )
        .unwrap();
    assert_eq!(cube.rows.len(), 3);

    let all_cube = eng
        .sql(
            "SELECT region, count(*) AS n FROM sales \
             GROUP BY ALL CUBE(region, region)",
            &[],
        )
        .unwrap();
    assert_eq!(all_cube.rows.len(), 7);
}

#[test]
fn group_by_distinct_uses_analyzed_expression_identity() {
    let eng = setup();
    let distinct_literals = eng
        .sql(
            "SELECT count(*) AS n FROM sales \
             GROUP BY DISTINCT GROUPING SETS ((amount + 1), (amount + 1.0), (amount + 1.00))",
            &[],
        )
        .unwrap();
    assert_eq!(distinct_literals.rows.len(), 12);

    let parenthesized = eng
        .sql(
            "SELECT count(*) AS n FROM sales \
             GROUP BY DISTINCT GROUPING SETS ((amount + 1), (((amount + 1))))",
            &[],
        )
        .unwrap();
    assert_eq!(parenthesized.rows.len(), 4);

    let empty = eng
        .sql(
            "SELECT count(*) AS n FROM sales WHERE false \
             GROUP BY DISTINCT GROUPING SETS ((), ())",
            &[],
        )
        .unwrap();
    assert_eq!(empty.rows.len(), 1);

    let explicit_rows = eng
        .sql(
            "SELECT count(*) AS n FROM sales \
             GROUP BY DISTINCT GROUPING SETS ((ROW(region, product)), (ROW(product, region)))",
            &[],
        )
        .unwrap();
    assert_eq!(explicit_rows.rows.len(), 8);

    let analyzed_casts = eng
        .sql(
            "SELECT count(*) AS n FROM sales \
             GROUP BY DISTINCT GROUPING SETS \
             ((amount + 1), (amount + 1::integer), (amount + 1::bigint))",
            &[],
        )
        .unwrap();
    assert_eq!(analyzed_casts.rows.len(), 8);

    let qualified_column = eng
        .sql(
            "SELECT count(*) AS n FROM sales AS s \
             GROUP BY DISTINCT GROUPING SETS ((amount), (s.amount))",
            &[],
        )
        .unwrap();
    assert_eq!(qualified_column.rows.len(), 4);

    let qualified_function = eng
        .sql(
            "SELECT count(*) AS n FROM sales \
             GROUP BY DISTINCT GROUPING SETS ((lower(region)), (pg_catalog.lower(region)))",
            &[],
        )
        .unwrap();
    assert_eq!(qualified_function.rows.len(), 2);

    let typed_null = eng
        .sql(
            "SELECT count(*) AS n FROM sales \
             GROUP BY DISTINCT GROUPING SETS ((amount + NULL), (amount + NULL::integer))",
            &[],
        )
        .unwrap();
    assert_eq!(typed_null.rows.len(), 1);

    let function_nulls = eng
        .sql(
            "SELECT count(*) AS n FROM sales \
             GROUP BY DISTINCT GROUPING SETS \
             ((lower(NULL)), (lower(NULL::text)), (region || NULL), (region || NULL::text))",
            &[],
        )
        .unwrap();
    assert_eq!(function_nulls.rows.len(), 2);
}
