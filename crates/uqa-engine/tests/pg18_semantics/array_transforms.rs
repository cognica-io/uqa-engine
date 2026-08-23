//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 `array_sort` and `array_reverse` parity.

use super::*;
use uqa_engine::SQLParam;

#[test]
fn pg18_array_sort_and_reverse() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[3,NULL,1,2])"),
        array(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Null,
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[3,NULL,1,2], true)"),
        array(vec![
            Value::Null,
            Value::Int(3),
            Value::Int(2),
            Value::Int(1),
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[3,NULL,1,2], false, true)"),
        array(vec![
            Value::Null,
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_reverse(ARRAY[[1,2],[3,4]])"),
        array(vec![
            Value::List(vec![Value::Int(3), Value::Int(4)]),
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        ])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[ARRAY[1,NULL],ARRAY[1,2]])"),
        array(vec![
            Value::List(vec![Value::Int(1), Value::Int(2)]),
            Value::List(vec![Value::Int(1), Value::Null]),
        ])
    );
}

#[test]
fn pg18_array_transforms_bind_polymorphic_and_named_arguments() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[3,1], 'true')"),
        array(vec![Value::Int(3), Value::Int(1)])
    );
    assert_eq!(
        scalar(&eng, "SELECT pg_catalog.array_sort(ARRAY[3,1])"),
        array(vec![Value::Int(1), Value::Int(3)])
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT array_sort(descending => true, \"array\" => ARRAY[3,1])"
        ),
        array(vec![Value::Int(3), Value::Int(1)])
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT array_sort(ARRAY[3,NULL,1], nulls_first => true, descending => false)"
        ),
        array(vec![Value::Null, Value::Int(1), Value::Int(3)])
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT pg_typeof(array_sort(ARRAY[2::smallint,1::smallint]))"
        ),
        Value::Str("smallint[]".into())
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT array_reverse((SELECT ARRAY[1::bigint,2::bigint]))"
        ),
        array(vec![Value::Int(2), Value::Int(1)])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[2,1], NULL::boolean)"),
        Value::Null
    );

    let result = eng
        .sql(
            "SELECT array_sort(ARRAY[2,1], descending => $1)",
            &[SQLParam::Scalar(Value::Str("true".into()))],
        )
        .unwrap();
    assert_eq!(
        result.rows[0].values().next(),
        Some(&array(vec![Value::Int(2), Value::Int(1)]))
    );
    let result = eng
        .sql(
            "SELECT array_reverse($1)",
            &[SQLParam::Scalar(array(vec![Value::Int(1), Value::Int(2)]))],
        )
        .unwrap();
    assert_eq!(
        result.rows[0].values().next(),
        Some(&array(vec![Value::Int(2), Value::Int(1)]))
    );
}

#[test]
fn pg18_array_transform_user_overloads_participate_in_resolution() {
    let eng = engine();
    for sql in [
        "CREATE FUNCTION array_sort(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT value::text'",
        "CREATE FUNCTION array_sort(value INTEGER[]) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user'''",
    ] {
        eng.sql(sql, &[]).unwrap();
    }
    assert_eq!(scalar(&eng, "SELECT array_sort(7)"), Value::Str("7".into()));
    assert_eq!(
        scalar(&eng, "SELECT array_sort(ARRAY[2,1])"),
        Value::Str("user".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT pg_catalog.array_sort(ARRAY[2,1])"),
        array(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT array_sort(\"array\" => ARRAY[2,1], descending => true)"
        ),
        array(vec![Value::Int(2), Value::Int(1)])
    );

    let ambiguous = engine();
    ambiguous
        .sql(
            "CREATE FUNCTION array_sort(value BIGINT[]) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''user'''",
            &[],
        )
        .unwrap();
    let error = ambiguous
        .sql("SELECT array_sort(ARRAY[2,1])", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42725"));
    assert_eq!(
        error.to_string(),
        "function array_sort(integer[]) is not unique"
    );
}

#[test]
fn pg18_array_transforms_report_exact_overload_errors() {
    let eng = engine();
    for (sql, sqlstate, message) in [
        (
            "SELECT array_sort()",
            "42883",
            "function array_sort() does not exist",
        ),
        (
            "SELECT array_sort(1)",
            "42883",
            "function array_sort(integer) does not exist",
        ),
        (
            "SELECT pg_catalog.array_sort(1)",
            "42883",
            "function pg_catalog.array_sort(integer) does not exist",
        ),
        (
            "SELECT array_sort(ARRAY[1], 1)",
            "42883",
            "function array_sort(integer[], integer) does not exist",
        ),
        (
            "SELECT array_reverse(ARRAY[1], true)",
            "42883",
            "function array_reverse(integer[], boolean) does not exist",
        ),
        (
            "SELECT array_sort(ARRAY[1], true, false, true)",
            "42883",
            "function array_sort(integer[], boolean, boolean, boolean) does not exist",
        ),
        (
            "SELECT array_sort(ARRAY[1], bad => true)",
            "42883",
            "function array_sort(integer[], bad => boolean) does not exist",
        ),
        (
            "SELECT array_sort(NULL, 1)",
            "42883",
            "function array_sort(unknown, integer) does not exist",
        ),
        (
            "SELECT array_sort(1, NULL)",
            "42883",
            "function array_sort(integer, unknown) does not exist",
        ),
        (
            "SELECT array_sort((SELECT 1))",
            "42883",
            "function array_sort(integer) does not exist",
        ),
        (
            "SELECT array_sort(NULL)",
            "42804",
            "could not determine polymorphic type because input has type unknown",
        ),
        (
            "SELECT array_reverse('{}')",
            "42804",
            "could not determine polymorphic type because input has type unknown",
        ),
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}: {error}");
        assert_eq!(error.to_string(), message, "{sql}");
    }
    let error = eng
        .sql("SELECT array_sort(ARRAY[2,1], 'not-a-boolean')", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22P02"));
    assert_eq!(
        error.to_string(),
        "invalid input syntax for type boolean: \"not-a-boolean\""
    );
    let error = eng
        .sql(
            "SELECT array_sort(ARRAY[2,1], $1)",
            &[SQLParam::Scalar(Value::Str("not-a-boolean".into()))],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22P02"));
    assert_eq!(
        error.to_string(),
        "invalid input syntax for type boolean: \"not-a-boolean\""
    );
    let error = eng
        .sql(
            "SELECT array_sort(ARRAY[2,1], $1::text)",
            &[SQLParam::Scalar(Value::Str("true".into()))],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(
        error.to_string(),
        "function array_sort(integer[], text) does not exist"
    );
}

#[test]
fn pg18_array_sort_requires_postgresql_element_comparators() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT array_sort('{}'::json[])::text"),
        Value::Str("{}".into())
    );
    assert!(eng.sql("SELECT array_sort(ARRAY['{}'::json])", &[]).is_ok());
    assert!(eng
        .sql(
            "SELECT array_reverse(ARRAY['{\"a\":1}'::json,'{\"a\":2}'::json])",
            &[]
        )
        .is_ok());
    for (sql, sqlstate) in [
        ("SELECT array_sort(ARRAY['{}'::json,'{}'::json])", "0A000"),
        ("SELECT array_sort(ARRAY[NULL,NULL]::json[])", "0A000"),
        (
            "SELECT array_sort(ARRAY[ARRAY['{}'::json],ARRAY['{}'::json]])",
            "42883",
        ),
        (
            "SELECT array_sort(ARRAY[ROW(1,'{}'::json),ROW(1,'{}'::json)])",
            "42883",
        ),
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}: {error}");
        assert_eq!(
            error.to_string(),
            "could not identify a comparison function for type json",
            "{sql}"
        );
    }
    assert!(eng
        .sql(
            "SELECT array_sort(ARRAY[ROW(2,'{}'::json),ROW(1,'{}'::json)])",
            &[]
        )
        .is_ok());
}
