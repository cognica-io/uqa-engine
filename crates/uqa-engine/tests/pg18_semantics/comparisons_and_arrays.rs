//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Comparison, pattern-matching, array, and set-returning parity tests.

use super::*;

// ---------------------------------------------------------------------
// IS DISTINCT FROM / row comparisons / SIMILAR TO / regex operators
// ---------------------------------------------------------------------

#[test]
fn is_distinct_from_is_null_safe() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT NULL IS DISTINCT FROM NULL"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 1 IS DISTINCT FROM NULL"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 1 IS DISTINCT FROM 2"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 1 IS DISTINCT FROM 1"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT NULL IS NOT DISTINCT FROM NULL"),
        Value::Bool(true)
    );
}

#[test]
fn row_comparisons_are_lexicographic() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT (1, 2) < (1, 3)"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT (1, 2) = (1, 2)"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT (1, NULL) = (1, 2)"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT (1, NULL) = (2, 2)"),
        Value::Bool(false)
    );
    assert_eq!(scalar(&eng, "SELECT (1, 2) < (1, NULL)"), Value::Null);
    // The first element decides before the NULL is reached.
    assert_eq!(
        scalar(&eng, "SELECT (2, 2) < (1, NULL)"),
        Value::Bool(false)
    );
}

#[test]
fn similar_to_translates_sql_regex() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT 'abc' SIMILAR TO 'a(b|c)c'"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 'abc' SIMILAR TO '%(b|d)%'"),
        Value::Bool(true)
    );
    // Anchored over the whole string.
    assert_eq!(
        scalar(&eng, "SELECT 'abc' SIMILAR TO '(b|c)%'"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 'abc' SIMILAR TO 'a_c'"),
        Value::Bool(true)
    );
    // Dot is a literal character in SQL regexes.
    assert_eq!(
        scalar(&eng, "SELECT 'a.c' SIMILAR TO 'a.c'"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 'axc' SIMILAR TO 'a.c'"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 'abc' NOT SIMILAR TO 'a_c'"),
        Value::Bool(false)
    );
}

#[test]
fn posix_regex_operators() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT 'abc' ~ 'a.c'"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT 'abc' ~ 'B'"), Value::Bool(false));
    assert_eq!(scalar(&eng, "SELECT 'abc' ~* 'A.C'"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT 'abc' !~ 'x'"), Value::Bool(true));
    assert_eq!(scalar(&eng, "SELECT 'abc' !~* 'A.C'"), Value::Bool(false));
    assert_eq!(scalar(&eng, "SELECT NULL ~ 'x'"), Value::Null);
}

#[test]
fn between_symmetric_swaps_bounds() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT 2 BETWEEN SYMMETRIC 3 AND 1"),
        Value::Bool(true)
    );
    assert_eq!(scalar(&eng, "SELECT 2 BETWEEN 3 AND 1"), Value::Bool(false));
    assert_eq!(
        scalar(&eng, "SELECT 4 BETWEEN SYMMETRIC 3 AND 1"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 2 BETWEEN SYMMETRIC NULL AND 1"),
        Value::Null
    );
    assert_eq!(
        scalar(&eng, "SELECT 2 NOT BETWEEN SYMMETRIC 3 AND 1"),
        Value::Bool(false)
    );
}

#[test]
fn any_all_over_arrays_are_three_valued() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT 2 = ANY(ARRAY[1,2,3])"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 5 = ANY(ARRAY[1,2,3])"),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(&eng, "SELECT 5 <> ALL(ARRAY[1,2,3])"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 1 = ANY(ARRAY[1, NULL])"),
        Value::Bool(true)
    );
    assert_eq!(scalar(&eng, "SELECT 3 = ANY(ARRAY[1, NULL])"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT 3 <> ALL(ARRAY[1, NULL])"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT 3 <> ALL(ARRAY[3, NULL])"),
        Value::Bool(false)
    );
    assert_eq!(scalar(&eng, "SELECT NULL = ANY(ARRAY[1, 2])"), Value::Null);
}

#[test]
fn array_subscripts_and_slices() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT (ARRAY[1, 2, 3])[2]"), Value::Int(2));
    assert_eq!(scalar(&eng, "SELECT (ARRAY[1, 2, 3])[0]"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT (ARRAY[1, 2, 3])[4]"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT (ARRAY[1, 2, 3])[1:2]"),
        array(vec![Value::Int(1), Value::Int(2)])
    );
    assert_eq!(
        scalar(&eng, "SELECT (ARRAY[1, 2, 3])[2:]"),
        array(vec![Value::Int(2), Value::Int(3)])
    );
    assert_eq!(
        scalar(&eng, "SELECT (regexp_match('foo123', '[0-9]+'))[1]"),
        Value::Str("123".into())
    );
}

#[test]
fn array_bounds_survive_storage_sorting_and_dimension_aware_access() {
    let eng = engine();
    let sorted = bounded_array(vec![Value::Int(1), Value::Int(2), Value::Int(3)], vec![0]);
    assert_eq!(
        scalar(&eng, "SELECT array_sort('[0:2]={3,1,2}'::int[])"),
        sorted
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT array_dims(array_sort('[0:2]={3,1,2}'::int[]))"
        ),
        Value::Str("[0:2]".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT array_lower('[0:2]={3,1,2}'::int[], 1)"),
        Value::Int(0)
    );
    assert_eq!(
        scalar(&eng, "SELECT array_upper('[0:2]={3,1,2}'::int[], 1)"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(&eng, "SELECT ('[0:2]={3,1,2}'::int[])[0]"),
        Value::Int(3)
    );

    eng.sql("CREATE TABLE bounded_arrays (v int[])", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO bounded_arrays VALUES ('[0:2]={3,1,2}'::int[])",
        &[],
    )
    .unwrap();
    assert_eq!(
        scalar(&eng, "SELECT v FROM bounded_arrays"),
        bounded_array(vec![Value::Int(3), Value::Int(1), Value::Int(2)], vec![0],)
    );

    assert_eq!(scalar(&eng, "SELECT (ARRAY[[1,2],[3,4]])[1]"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT (ARRAY[[1,2],[3,4]])[1][2]"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(&eng, "SELECT (ARRAY[[1,2],[3,4]])[1:1][2]"),
        array(vec![Value::List(vec![Value::Int(1), Value::Int(2)])])
    );
}

#[test]
fn array_length_of_empty_array_is_null() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT array_length(ARRAY[]::int[], 1)"),
        Value::Null
    );
    assert_eq!(
        scalar(&eng, "SELECT array_length(ARRAY[1,2,3], 1)"),
        Value::Int(3)
    );
    assert_eq!(
        scalar(&eng, "SELECT cardinality(ARRAY[]::int[])"),
        Value::Int(0)
    );
}

#[test]
fn srf_in_select_list_expands_rows() {
    let eng = engine();
    let result = eng.sql("SELECT generate_series(1, 3)", &[]).unwrap();
    assert_eq!(result.rows.len(), 3);
    let result = eng
        .sql("SELECT jsonb_object_keys('{\"b\":1,\"a\":2}'::jsonb)", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn interval_comparison_and_ordering() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT interval '1 day' < interval '25 hours'"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT interval '1 mon' = interval '30 days'"),
        Value::Bool(true)
    );
}

#[test]
fn interval_column_round_trip() {
    let eng = engine();
    // Interval values survive projection through expressions.
    assert_eq!(
        scalar(&eng, "SELECT (interval '1 day' + interval '1 hour') * 2"),
        Value::Temporal(TemporalValue::Interval {
            months: 0,
            days: 2,
            micros: 2 * 3_600 * 1_000_000,
        })
    );
}
