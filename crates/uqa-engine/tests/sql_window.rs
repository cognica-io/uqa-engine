//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window functions over PARTITION BY + ORDER BY clauses
//! (`ROW_NUMBER`, `RANK`, `DENSE_RANK`, `LAG`, `LEAD`, `NTILE`).

use uqa_core::Value;
use uqa_engine::Engine;

fn corpus() -> Engine {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE sales (id INTEGER PRIMARY KEY, rep TEXT, region TEXT, amount INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO sales (id, rep, region, amount) VALUES \
         (1, 'alice', 'east', 100), \
         (2, 'alice', 'east', 200), \
         (3, 'alice', 'east', 200), \
         (4, 'bob',   'east', 150), \
         (5, 'bob',   'east', 50),  \
         (6, 'carol', 'west', 80),  \
         (7, 'carol', 'west', 120)",
        &[],
    )
    .unwrap();
    eng
}

fn ints(rows: &[uqa_engine::SQLResult], col: &str) -> Vec<i64> {
    rows.iter()
        .flat_map(|r| r.rows.iter())
        .filter_map(|row| row.get(col).cloned())
        .filter_map(|v| match v {
            Value::Int(n) => Some(n),
            _ => None,
        })
        .collect()
}

#[test]
fn row_number_over_partition() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT id, \
                    row_number() OVER (PARTITION BY rep ORDER BY amount DESC) AS rn \
             FROM sales \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let rns = ints(&[r], "rn");
    // alice (3 rows by amount desc -> ids 2,3,1 get rn 1,2,3, but emitted in id order)
    // alice rn for id=1 should be 3 (smallest amount), id=2 and id=3 share amount=200 so
    // their ordering depends on stable sort of ties — we accept either 1/2 ordering.
    assert_eq!(rns.len(), 7);
    // Bob: id=4 (150) -> rn 1, id=5 (50) -> rn 2.
    let id_to_rn: std::collections::BTreeMap<i64, i64> = (1..=7).zip(rns.iter().copied()).collect();
    assert_eq!(id_to_rn.get(&4), Some(&1));
    assert_eq!(id_to_rn.get(&5), Some(&2));
    // Carol: id=6 (80) -> rn 2, id=7 (120) -> rn 1.
    assert_eq!(id_to_rn.get(&7), Some(&1));
    assert_eq!(id_to_rn.get(&6), Some(&2));
    // Alice: id=1 (100) -> rn 3 (smallest in alice's partition).
    assert_eq!(id_to_rn.get(&1), Some(&3));
}

#[test]
fn window_slots_cannot_alias_user_column_names() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE window_name_collision (id INTEGER, __window_0_0 BIGINT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO window_name_collision VALUES (2, 99), (1, 88)",
        &[],
    )
    .unwrap();

    let result = eng
        .sql(
            "SELECT row_number() OVER (ORDER BY id) + __window_0_0 AS total FROM window_name_collision ORDER BY id",
            &[],
        )
        .unwrap();

    assert_eq!(result.rows[0]["total"], Value::Int(89));
    assert_eq!(result.rows[1]["total"], Value::Int(101));
    assert!(result.rows.iter().all(|row| row.len() == 1));
}

#[test]
fn named_windows_share_and_extend_definitions() {
    let eng = corpus();
    let result = eng
        .sql(
            "SELECT id, \
                    row_number() OVER ranked AS rn, \
                    sum(amount) OVER running AS running_total \
             FROM sales \
             WINDOW base AS (PARTITION BY rep), \
                    ranked AS (base ORDER BY amount DESC, id), \
                    running AS (base ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let expected = [
        (1, 3, 100),
        (2, 1, 300),
        (3, 2, 500),
        (4, 1, 150),
        (5, 2, 200),
        (6, 2, 80),
        (7, 1, 200),
    ];
    assert_eq!(result.rows.len(), expected.len());
    for (row, (id, rank, running_total)) in result.rows.iter().zip(expected) {
        assert_eq!(row.get("id"), Some(&Value::Int(id)));
        assert_eq!(row.get("rn"), Some(&Value::Int(rank)));
        assert_eq!(row.get("running_total"), Some(&Value::Int(running_total)));
    }
}

#[test]
fn pg18_builtin_aggregate_windows_bind_over_values_sources() {
    let eng = Engine::new();
    let result = eng
        .sql(
            "SELECT sum(x) OVER (ORDER BY x) AS running FROM (VALUES (1), (2), (3)) AS t(x)",
            &[],
        )
        .unwrap();

    assert_eq!(ints(&[result], "running"), vec![1, 3, 6]);
}

#[test]
fn row_number_inside_projection_expression() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT row_number() OVER (ORDER BY id) - 1 AS zero_based
             FROM sales
             ORDER BY zero_based
             LIMIT 3",
            &[],
        )
        .unwrap();

    let got: Vec<i64> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("zero_based") {
            Some(Value::Int(value)) => Some(*value),
            _ => None,
        })
        .collect();
    assert_eq!(got, vec![0, 1, 2]);
}

#[test]
fn rank_assigns_ties_then_skips() {
    let eng = corpus();
    // Rank within rep=alice by amount DESC. ids 2 and 3 share amount=200.
    let r = eng
        .sql(
            "SELECT id, \
                    rank() OVER (PARTITION BY rep ORDER BY amount DESC) AS rnk \
             FROM sales \
             WHERE rep = 'alice' \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let id_to_rnk: std::collections::BTreeMap<i64, i64> = r
        .rows
        .iter()
        .filter_map(|row| match (row.get("id"), row.get("rnk")) {
            (Some(Value::Int(i)), Some(Value::Int(k))) => Some((*i, *k)),
            _ => None,
        })
        .collect();
    // ids 2 and 3 both rank 1 (amount=200), id 1 (amount=100) gets rank 3 — gap after tie.
    assert_eq!(id_to_rnk.get(&2), Some(&1));
    assert_eq!(id_to_rnk.get(&3), Some(&1));
    assert_eq!(id_to_rnk.get(&1), Some(&3));
}

#[test]
fn dense_rank_no_gap_after_tie() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT id, \
                    dense_rank() OVER (PARTITION BY rep ORDER BY amount DESC) AS dr \
             FROM sales \
             WHERE rep = 'alice' \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let id_to_dr: std::collections::BTreeMap<i64, i64> = r
        .rows
        .iter()
        .filter_map(|row| match (row.get("id"), row.get("dr")) {
            (Some(Value::Int(i)), Some(Value::Int(k))) => Some((*i, *k)),
            _ => None,
        })
        .collect();
    assert_eq!(id_to_dr.get(&2), Some(&1));
    assert_eq!(id_to_dr.get(&3), Some(&1));
    assert_eq!(id_to_dr.get(&1), Some(&2)); // dense rank: no gap
}

#[test]
fn lag_returns_prior_row_in_partition() {
    let eng = corpus();
    let r = eng
        .sql(
            "SELECT id, \
                    lag(amount, 1, 0) OVER (PARTITION BY rep ORDER BY id) AS prev \
             FROM sales \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let id_to_prev: std::collections::BTreeMap<i64, i64> = r
        .rows
        .iter()
        .filter_map(|row| match (row.get("id"), row.get("prev")) {
            (Some(Value::Int(i)), Some(Value::Int(k))) => Some((*i, *k)),
            _ => None,
        })
        .collect();
    // alice partition: id 1 first -> prev=0; id 2 -> prev=100; id 3 -> prev=200.
    assert_eq!(id_to_prev.get(&1), Some(&0));
    assert_eq!(id_to_prev.get(&2), Some(&100));
    assert_eq!(id_to_prev.get(&3), Some(&200));
    // bob: id 4 -> 0, id 5 -> 150.
    assert_eq!(id_to_prev.get(&4), Some(&0));
    assert_eq!(id_to_prev.get(&5), Some(&150));
}

#[test]
fn ntile_buckets_partition() {
    let eng = corpus();
    // Alice has 3 rows -> ntile(2) -> bucket sizes 2, 1.
    let r = eng
        .sql(
            "SELECT id, \
                    ntile(2) OVER (PARTITION BY rep ORDER BY id) AS bucket \
             FROM sales \
             WHERE rep = 'alice' \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let id_to_bucket: std::collections::BTreeMap<i64, i64> = r
        .rows
        .iter()
        .filter_map(|row| match (row.get("id"), row.get("bucket")) {
            (Some(Value::Int(i)), Some(Value::Int(k))) => Some((*i, *k)),
            _ => None,
        })
        .collect();
    assert_eq!(id_to_bucket.get(&1), Some(&1));
    assert_eq!(id_to_bucket.get(&2), Some(&1));
    assert_eq!(id_to_bucket.get(&3), Some(&2));
}

#[test]
fn star_with_window_projection_preserves_every_source_position() {
    let eng = corpus();
    for star in ["*", "sales.*"] {
        let result = eng
            .sql(
                &format!(
                    "SELECT {star}, row_number() OVER (ORDER BY id) AS rn
                     FROM sales
                     WHERE id <= 2
                     ORDER BY id"
                ),
                &[],
            )
            .unwrap();
        assert_eq!(result.columns, ["id", "rep", "region", "amount", "rn"]);
        assert_eq!(result.value_at(0, 0), Some(&Value::Int(1)));
        assert_eq!(result.value_at(0, 1), Some(&Value::Str("alice".into())));
        assert_eq!(result.value_at(0, 2), Some(&Value::Str("east".into())));
        assert_eq!(result.value_at(0, 3), Some(&Value::Int(100)));
        assert_eq!(result.value_at(0, 4), Some(&Value::Int(1)));
        assert_eq!(result.value_at(1, 0), Some(&Value::Int(2)));
        assert_eq!(result.value_at(1, 4), Some(&Value::Int(2)));
    }
}
