//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
