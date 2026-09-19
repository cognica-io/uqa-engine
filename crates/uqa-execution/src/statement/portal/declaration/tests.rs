//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::NoRoutines;

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query fixture produced a command");
    };
    *query
}

#[test]
fn unspecified_scroll_requires_native_backward_traversal() {
    for (sql, expected) in [
        ("SELECT 1", false),
        ("SELECT count(*) FROM generate_series(1, 2)", false),
        ("SELECT generate_series(1, 2)", false),
        (
            "SELECT row_number() OVER () FROM generate_series(1, 2)",
            false,
        ),
        ("SELECT * FROM (SELECT 1 AS n) AS source", false),
        ("VALUES (1), (2)", true),
        ("SELECT * FROM generate_series(1, 2)", true),
        ("SELECT * FROM rows FOR UPDATE", false),
        ("SELECT * FROM rows", true),
    ] {
        assert_eq!(
            query_scrollable(&NoRoutines, &query(sql), None),
            expected,
            "{sql}"
        );
    }
}

#[test]
fn explicit_scroll_keeps_the_requested_traversal_and_materialization_contract() {
    for sql in [
        "SELECT 1",
        "VALUES (1), (2)",
        "SELECT count(*) FROM generate_series(1, 2)",
    ] {
        let query = query(sql);
        assert!(query_scrollable(&NoRoutines, &query, Some(true)), "{sql}");
        assert!(!query_scrollable(&NoRoutines, &query, Some(false)), "{sql}");
    }
}
