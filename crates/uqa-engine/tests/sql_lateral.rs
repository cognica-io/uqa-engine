//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `LATERAL` join: right side re-evaluates per left row and the ON
//! predicate sees both sides.

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn lateral_cross_join_with_generate_series() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id, n) VALUES (1, 2), (2, 3)", &[])
        .unwrap();
    let r = eng
        .sql(
            "SELECT t.id, gs.gs
             FROM t, LATERAL generate_series(1, t.n) AS gs
             ORDER BY t.id, gs.gs",
            &[],
        )
        .unwrap();
    assert_eq!(
        int_pairs(&r, "id", "gs"),
        vec![(1, 1), (1, 2), (2, 1), (2, 2), (2, 3)]
    );
}

#[test]
fn range_function_can_reference_left_from_item_without_lateral_keyword() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE params (id INTEGER PRIMARY KEY, width INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO params (id, width) VALUES (1, 2), (2, 3)", &[])
        .unwrap();
    let r = eng
        .sql(
            "SELECT p.id, x
             FROM params p, generate_series(0, p.width - 1) AS gs(x)
             ORDER BY p.id, x",
            &[],
        )
        .unwrap();
    let mut rows = int_pairs(&r, "id", "x");
    rows.sort_unstable();
    assert_eq!(rows, vec![(1, 0), (1, 1), (2, 0), (2, 1), (2, 2)]);
}

fn int_pairs(result: &uqa_engine::SQLResult, left: &str, right: &str) -> Vec<(i64, i64)> {
    result
        .rows
        .iter()
        .map(|row| (int_col(row, left), int_col(row, right)))
        .collect()
}

fn int_col(row: &uqa_sql::ResultRow, column: &str) -> i64 {
    match row.get(column).expect(column) {
        Value::Int(value) => *value,
        other => panic!("{other:?}"),
    }
}
