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
