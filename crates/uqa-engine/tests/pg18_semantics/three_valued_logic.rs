//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Three-valued logic and row-constructor parity tests.

use super::*;

// ---------------------------------------------------------------------
// Three-valued logic
// ---------------------------------------------------------------------

#[test]
fn null_comparisons_yield_null() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT NULL = NULL"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT 1 = NULL"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT NULL <> 1"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT NULL < 1"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT NOT NULL"), Value::Null);
}

#[test]
fn row_constructors_are_records_and_keep_postgresql_null_comparison_semantics() {
    let eng = engine();

    assert_eq!(
        scalar(&eng, "SELECT ROW(1, 2)"),
        Value::Row(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(
        scalar(&eng, "SELECT pg_typeof(ROW(1, 2))"),
        Value::Str("record".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT pg_typeof(ARRAY[1, 2])"),
        Value::Str("integer[]".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT ROW(1, NULL) = ROW(1, NULL)"),
        Value::Null
    );
    assert_eq!(scalar(&eng, "SELECT ROW(1, NULL) < ROW(1, 2)"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT ARRAY[1, NULL] = ARRAY[1, NULL]"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT ROW(1, NULL)::text"),
        Value::Str("(1,)".into())
    );
}

#[test]
fn in_list_three_valued() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT 3 IN (1, 2, NULL)"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT 3 NOT IN (1, 2, NULL)"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT 1 IN (1, NULL)"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT 3 NOT IN (1, 2)"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT NULL IN (1, 2)"), Value::Null);
}

#[test]
fn in_subquery_three_valued() {
    let eng = engine();
    eng.sql("CREATE TABLE in_values (v INTEGER)", &[]).unwrap();
    eng.sql("INSERT INTO in_values VALUES (1), (NULL)", &[])
        .unwrap();

    assert_eq!(
        scalar(&eng, "SELECT 3 IN (SELECT v FROM in_values)"),
        Value::Null
    );
    assert_eq!(
        scalar(&eng, "SELECT 3 NOT IN (SELECT v FROM in_values)"),
        Value::Null
    );
    assert_eq!(
        scalar(&eng, "SELECT 1 IN (SELECT v FROM in_values)"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT NULL IN (SELECT v FROM in_values)"),
        Value::Null
    );
    assert_eq!(
        scalar(&eng, "SELECT NULL NOT IN (SELECT v FROM in_values)"),
        Value::Null
    );

    assert_eq!(
        scalar(&eng, "SELECT 3 IN (SELECT v FROM in_values WHERE false)"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT NULL IN (SELECT v FROM in_values WHERE false)"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT NULL NOT IN (SELECT v FROM in_values WHERE false)"
        ),
        Value::Bool(true)
    );
}

#[test]
fn kleene_and_or() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT NULL AND false"), Value::Bool(false));
    assert_eq!(scalar(&eng, "SELECT NULL AND true"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT NULL OR true"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT NULL OR false"), Value::Null);
}

#[test]
fn between_three_valued() {
    let eng = engine();
    // A definite FALSE bound wins over the NULL bound.
    assert_eq!(
        scalar(&eng, "SELECT 2 BETWEEN 3 AND NULL"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 2 BETWEEN NULL AND 1"),
        Value::Bool(false)
    );
    assert_eq!(scalar(&eng, "SELECT 2 BETWEEN NULL AND 3"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT NULL BETWEEN 1 AND 2"), Value::Null);
}

#[test]
fn case_when_null_not_taken() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT CASE WHEN NULL THEN 1 ELSE 2 END"),
        Value::Int(2)
    );
}

#[test]
fn where_treats_null_as_no_match() {
    let eng = engine();
    eng.sql("CREATE TABLE t3vl (id INTEGER, v INTEGER)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO t3vl (id, v) VALUES (1, 5), (2, 7), (3, NULL)",
        &[],
    )
    .unwrap();
    let ids = |sql: &str| -> Vec<i64> {
        let mut out: Vec<i64> = eng
            .sql(sql, &[])
            .unwrap()
            .rows
            .iter()
            .filter_map(|row| match row.get("id") {
                Some(Value::Int(n)) => Some(*n),
                _ => None,
            })
            .collect();
        out.sort_unstable();
        out
    };
    assert_eq!(ids("SELECT id FROM t3vl WHERE v = 5"), vec![1]);
    // NOT (v = 5) must NOT match the NULL row (PostgreSQL 3VL).
    assert_eq!(ids("SELECT id FROM t3vl WHERE NOT (v = 5)"), vec![2]);
    assert_eq!(ids("SELECT id FROM t3vl WHERE v <> 5"), vec![2]);
    assert_eq!(ids("SELECT id FROM t3vl WHERE v NOT IN (5)"), vec![2]);
    assert_eq!(ids("SELECT id FROM t3vl WHERE v IS NULL"), vec![3]);
    assert_eq!(ids("SELECT id FROM t3vl WHERE v IS NOT NULL"), vec![1, 2]);
}

#[test]
fn select_without_from_honors_where() {
    let eng = engine();
    assert_eq!(eng.sql("SELECT 1 WHERE false", &[]).unwrap().rows.len(), 0);
    assert_eq!(eng.sql("SELECT 1 WHERE NULL", &[]).unwrap().rows.len(), 0);
    assert_eq!(eng.sql("SELECT 1 WHERE true", &[]).unwrap().rows.len(), 1);
    assert_eq!(
        scalar(&eng, "SELECT EXISTS (SELECT 1 WHERE false)"),
        Value::Bool(false)
    );
}
