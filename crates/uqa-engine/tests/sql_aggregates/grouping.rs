//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
    // PostgreSQL 18: float8 -> int casts round half to even, so
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

#[test]
fn having_can_use_an_aggregate_not_projected_by_select() {
    let eng = engine();
    eng.sql("CREATE TABLE hidden_having (cat TEXT, amount INTEGER)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO hidden_having (cat, amount) VALUES \
         ('a', 10), ('a', 20), ('b', 5)",
        &[],
    )
    .unwrap();

    let result = eng
        .sql(
            "SELECT cat FROM hidden_having GROUP BY cat \
             HAVING SUM(amount) > 20 ORDER BY cat",
            &[],
        )
        .unwrap();

    assert_eq!(result.columns, vec!["cat"]);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(str_col(&result.rows[0], "cat"), Some("a"));
}

#[test]
fn aggregate_order_and_distinct_on_keep_unprojected_group_keys() {
    let eng = engine();
    let ordered = eng
        .sql(
            "SELECT count(*) AS n FROM (VALUES (2), (1), (2)) AS input(x) GROUP BY x ORDER BY x",
            &[],
        )
        .unwrap();
    assert_eq!(ordered.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(ordered.value_at(1, 0), Some(&Value::Int(2)));

    let distinct = eng
        .sql(
            "SELECT DISTINCT ON (x) count(*) AS n FROM (VALUES (2), (1), (2)) AS input(x) GROUP BY x ORDER BY x",
            &[],
        )
        .unwrap();
    assert_eq!(distinct.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(distinct.value_at(1, 0), Some(&Value::Int(2)));
}

#[test]
fn grouped_star_expands_to_bound_source_columns() {
    let eng = engine_with_table();
    let bare = eng
        .sql("SELECT * FROM t GROUP BY id, val, name ORDER BY id", &[])
        .unwrap();
    assert_eq!(bare.columns, ["id", "val", "name"]);
    assert_eq!(bare.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(bare.value_at(0, 1), Some(&Value::Int(10)));
    assert_eq!(bare.value_at(0, 2), Some(&Value::Str("alpha".into())));

    let qualified = eng
        .sql(
            "SELECT t.*, count(*) AS c
             FROM t
             GROUP BY t.id, t.val, t.name
             ORDER BY t.id",
            &[],
        )
        .unwrap();
    assert_eq!(qualified.columns, ["id", "val", "name", "c"]);
    assert_eq!(qualified.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(qualified.value_at(0, 3), Some(&Value::Int(1)));
}
