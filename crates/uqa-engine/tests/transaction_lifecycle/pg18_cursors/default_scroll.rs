//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::Value;

#[test]
fn pg18_default_cursor_scroll_follows_native_plan_traversal() {
    let engine = Engine::new();
    for (query, scrollable) in [
        ("SELECT 1 AS n", false),
        ("SELECT count(*) AS n FROM generate_series(1, 2)", false),
        ("SELECT generate_series(1, 2) AS n", false),
        (
            "SELECT row_number() OVER () AS n FROM generate_series(1, 2)",
            false,
        ),
        ("SELECT * FROM (SELECT 1 AS n) AS source", false),
        ("VALUES (1), (2)", true),
        ("SELECT * FROM generate_series(1, 2)", true),
    ] {
        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql(&format!("DECLARE default_scroll CURSOR FOR {query}"), &[])
            .unwrap();
        assert!(!engine
            .sql("FETCH ALL FROM default_scroll", &[])
            .unwrap()
            .rows
            .is_empty());
        let backward = engine.sql("FETCH BACKWARD 1 FROM default_scroll", &[]);
        if scrollable {
            assert_eq!(backward.unwrap().rows.len(), 1, "{query}");
        } else {
            let error = backward.unwrap_err();
            assert_eq!(error.sqlstate(), Some("55000"), "{query}: {error}");
            assert_eq!(error.to_string(), "cursor can only scan forward", "{query}");
        }
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}

#[test]
fn pg18_explicit_scroll_preserves_backward_access_for_materialized_plans() {
    let engine = Engine::new();
    for query in [
        "SELECT 1 AS n",
        "SELECT count(*) AS n FROM generate_series(1, 2)",
        "SELECT generate_series(1, 2) AS n",
    ] {
        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql(
                &format!("DECLARE explicit_scroll SCROLL CURSOR FOR {query}"),
                &[],
            )
            .unwrap();
        assert!(!engine
            .sql("FETCH ALL FROM explicit_scroll", &[])
            .unwrap()
            .rows
            .is_empty());
        assert_eq!(
            engine
                .sql("FETCH BACKWARD 1 FROM explicit_scroll", &[])
                .unwrap()
                .rows
                .len(),
            1,
            "{query}"
        );
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}

#[test]
fn pg18_default_scroll_tracks_outer_ordering_and_cte_materialization() {
    let engine = Engine::new();
    let mut mismatches = Vec::new();
    for (query, expected) in [
        ("SELECT row_number() OVER () AS n FROM generate_series(1, 2) ORDER BY 1", true),
        ("SELECT generate_series(1, 2) AS n ORDER BY 1", true),
        ("SELECT x, generate_series(1, 2) FROM generate_series(1, 2) x ORDER BY x", false),
        ("SELECT DISTINCT x FROM generate_series(1, 2) x ORDER BY x", false),
        ("SELECT * FROM generate_series(1, 2) UNION ALL SELECT * FROM generate_series(3, 4) ORDER BY 1", true),
        ("SELECT * FROM generate_series(1, 2) UNION SELECT * FROM generate_series(3, 4) ORDER BY 1", false),
        ("WITH c AS (SELECT 1) SELECT * FROM c", false),
        ("WITH c AS MATERIALIZED (SELECT 1) SELECT * FROM c", true),
        ("SELECT x, row_number() OVER (ORDER BY x) FROM generate_series(1, 2) x ORDER BY x", false),
        ("SELECT x, row_number() OVER (ORDER BY x) FROM generate_series(1, 2) x ORDER BY x DESC", true),
        ("SELECT x, row_number() OVER (PARTITION BY x) FROM generate_series(1, 2) x ORDER BY x", false),
        ("SELECT row_number() OVER () AS n FROM generate_series(1, 2) ORDER BY n", true),
        ("SELECT generate_series(1, 2) AS n ORDER BY n", true),
        ("SELECT generate_series(1, 2) AS n ORDER BY generate_series(1, 2)", true),
        ("SELECT generate_series(1, 2), generate_series(3, 4) ORDER BY 1", true),
        ("SELECT *, generate_series(1, 2) FROM (VALUES (10, 20)) source(a, b) ORDER BY 3", true),
        ("SELECT *, generate_series(1, 2) FROM (VALUES (10, 20)) source(a, b) ORDER BY 2", false),
        ("SELECT 1 ORDER BY 1", false),
        ("WITH c AS (SELECT 1) SELECT * FROM c ORDER BY 1", false),
        ("SELECT 1 UNION ALL SELECT 2 ORDER BY 1", true),
        ("WITH c AS (SELECT * FROM generate_series(1, 2)) SELECT * FROM c", true),
        ("WITH c AS (SELECT count(*) FROM generate_series(1, 2)) SELECT * FROM c", false),
        ("WITH c AS (SELECT random()) SELECT * FROM c", true),
        ("WITH c AS NOT MATERIALIZED (SELECT random()) SELECT * FROM c", true),
        ("WITH c AS NOT MATERIALIZED (SELECT 1) SELECT * FROM c", false),
        ("WITH z AS (SELECT 1), a AS (SELECT * FROM z) SELECT * FROM a", false),
        ("WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 2) SELECT * FROM c", true),
        ("WITH c AS (SELECT 1) SELECT * FROM c UNION ALL SELECT * FROM c", true),
        ("WITH c AS NOT MATERIALIZED (SELECT 1) SELECT * FROM c UNION ALL SELECT * FROM c", false),
    ] {
        engine.sql("BEGIN", &[]).unwrap();
        engine.sql(&format!("DECLARE shaped_cursor CURSOR FOR {query}"), &[]).unwrap();
        let metadata = engine.sql("SELECT is_scrollable FROM pg_cursors WHERE name = 'shaped_cursor'", &[]).unwrap();
        let Some(Value::Bool(actual)) = metadata.value_at(0, 0) else {
            panic!("missing cursor metadata for {query}");
        };
        if *actual == expected {
            let all = engine.sql("FETCH ALL FROM shaped_cursor", &[]).unwrap_or_else(|error| panic!("{query}: {error}"));
            assert!(!all.rows.is_empty(), "{query}");
            let backward = engine.sql("FETCH BACKWARD 1 FROM shaped_cursor", &[]);
            if expected {
                let backward = backward.unwrap();
                assert_eq!(backward.rows.len(), 1, "{query}");
                for column in 0..all.columns.len() {
                    assert_eq!(backward.value_at(0, column), all.value_at(all.rows.len() - 1, column), "{query}");
                }
            } else {
                assert_eq!(backward.unwrap_err().sqlstate(), Some("55000"), "{query}");
            }
        } else {
            mismatches.push((query, *actual, expected));
        }
        engine.sql("ROLLBACK", &[]).unwrap();
    }
    assert!(
        mismatches.is_empty(),
        "default scroll differences: {mismatches:?}"
    );
}

#[test]
fn pg18_cursor_target_sets_preserve_sorting_zip_and_output_slicing() {
    let engine = Engine::new();
    engine.sql("CREATE SEQUENCE srf_order", &[]).unwrap();
    for (query, expected) in [
        ("SELECT generate_series(2, 4) AS n ORDER BY 1 OFFSET 1 LIMIT 1", vec![vec![Some(3)]]),
        ("SELECT generate_series(1, 2) AS a, generate_series(10, 12) AS b ORDER BY 2", vec![vec![Some(1), Some(10)], vec![Some(2), Some(11)], vec![None, Some(12)]]),
        ("SELECT x, generate_series(1, 2) AS n FROM (VALUES (2), (1)) t(x) ORDER BY x OFFSET 1 LIMIT 2", vec![vec![Some(1), Some(2)], vec![Some(2), Some(1)]]),
        ("SELECT x, generate_series(1, 2) AS n FROM (VALUES (1), (1), (2)) t(x) ORDER BY x FETCH FIRST 1 ROW WITH TIES", vec![vec![Some(1), Some(1)], vec![Some(1), Some(2)], vec![Some(1), Some(1)], vec![Some(1), Some(2)]]),
        ("SELECT x, generate_series(nextval('srf_order'), nextval('srf_order')) AS n FROM (VALUES (2), (1)) t(x) ORDER BY x", vec![vec![Some(1), Some(1)], vec![Some(1), Some(2)], vec![Some(2), Some(3)], vec![Some(2), Some(4)]]),
    ] {
        engine.sql("BEGIN", &[]).unwrap();
        engine.sql(&format!("DECLARE set_cursor CURSOR FOR {query}"), &[]).unwrap();
        let result = engine.sql("FETCH ALL FROM set_cursor", &[]).unwrap_or_else(|error| panic!("{query}: {error}"));
        assert_eq!(result.rows.len(), expected.len(), "{query}");
        for (row, expected) in expected.into_iter().enumerate() {
            assert_eq!(result.columns.len(), expected.len(), "{query}");
            for (column, expected) in expected.into_iter().enumerate() {
                assert_eq!(result.value_at(row, column), Some(&expected.map_or(Value::Null, Value::Int)), "{query}: row {row}, column {column}");
            }
        }
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}
