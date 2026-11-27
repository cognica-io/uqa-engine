//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Comparison, pattern-matching, array, and set-returning parity tests.

use super::*;

#[test]
fn array_literal_diagnostics_match_postgresql() {
    crate::pg18_oracle::verify(
        &engine(),
        include_str!(
            "../../../../tests/parity/pg18/array_literal_diagnostics_oracle.expected.json"
        ),
    );
}

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

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn distinct_predicates_in_parameterized_mutations(#[case] provider: usize) {
    use uqa_engine::SQLParam;

    let directory = tempfile::tempdir().unwrap();
    let engine = crate::open_test_provider(provider, &directory.path().join("distinct.db"));
    for sql in [
        "CREATE TABLE slots (slot_id bigint PRIMARY KEY, owner_id text, lease_token text)",
        "INSERT INTO slots VALUES (1, 'm', 'a'), (2, 'm', NULL)",
        "CREATE TABLE commit_lock (singleton bigint PRIMARY KEY, token text)",
        "INSERT INTO commit_lock VALUES (1, NULL)",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    let update = "UPDATE slots SET owner_id=$1 WHERE lease_token IS NOT DISTINCT FROM $2";
    let delete = "DELETE FROM slots WHERE slot_id=99 AND lease_token IS NOT DISTINCT FROM $1";
    let nested = "UPDATE commit_lock SET token=$1 WHERE singleton=1 AND EXISTS (SELECT 1 FROM slots WHERE slot_id=1 AND lease_token IS NOT DISTINCT FROM $2)";
    let distinct =
        "UPDATE slots SET owner_id=$1 WHERE slot_id=99 AND lease_token IS DISTINCT FROM $2";
    for (sql, parameters, tag) in [
        (update, &[Some("matched"), Some("a")][..], "UPDATE 1"),
        (delete, &[Some("a")], "DELETE 0"),
        (nested, &[Some("t"), Some("a")], "UPDATE 1"),
        (distinct, &[Some("unused"), Some("a")], "UPDATE 0"),
        (update, &[Some("was_null"), None], "UPDATE 1"),
        (delete, &[None], "DELETE 0"),
        (nested, &[Some("unused"), None], "UPDATE 0"),
        (distinct, &[Some("unused"), None], "UPDATE 0"),
        (
            "DELETE FROM slots WHERE $1 IS DISTINCT FROM lease_token",
            &[None],
            "DELETE 1",
        ),
    ] {
        let params: Vec<_> = parameters
            .iter()
            .map(|value| {
                SQLParam::scalar(value.map_or(Value::Null, |text| Value::Str(text.into())))
            })
            .collect();
        let result = engine
            .sql_batch(&[(sql, &params)])
            .unwrap_or_else(|error| panic!("{sql} {parameters:?}: {error}"));
        assert_eq!(
            result[0].command_tag.as_deref(),
            Some(tag),
            "{sql} {parameters:?}"
        );
    }
    assert_eq!(scalar(&engine, "SELECT slot_id FROM slots"), Value::Int(2));
    assert_eq!(
        scalar(&engine, "SELECT owner_id FROM slots"),
        Value::Str("was_null".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT token FROM commit_lock"),
        Value::Str("t".into())
    );
}

#[test]
fn distinct_mutations_compare_vector_and_tensor_columns() {
    use uqa_engine::SQLParam;

    let eng = engine();
    eng.sql(
        "CREATE TABLE items (id integer, v vector(2), t tensor(2))",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO items VALUES (1, $1, $2), (2, NULL, NULL)",
        &[
            SQLParam::Vector(vec![1.0, 2.0]),
            SQLParam::Tensor(vec![vec![3.0, 4.0]]),
        ],
    )
    .unwrap();
    let null = [SQLParam::scalar(Value::Null)];
    for (sql, params, tag) in [
        (
            "UPDATE items SET id=id WHERE v IS NOT DISTINCT FROM v",
            &[][..],
            "UPDATE 2",
        ),
        (
            "UPDATE items SET id=id WHERE t IS DISTINCT FROM t",
            &[],
            "UPDATE 0",
        ),
        (
            "UPDATE items SET id=id WHERE $1 IS NOT DISTINCT FROM v",
            &null,
            "UPDATE 1",
        ),
        (
            "DELETE FROM items WHERE t IS DISTINCT FROM $1",
            &null,
            "DELETE 1",
        ),
    ] {
        let result = eng
            .sql_batch(&[(sql, params)])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(result[0].command_tag.as_deref(), Some(tag), "{sql}");
    }
    assert_eq!(scalar(&eng, "SELECT id FROM items"), Value::Int(2));
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
