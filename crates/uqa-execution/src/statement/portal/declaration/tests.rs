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

fn scrollable(query: &QueryPlan, requested: Option<bool>) -> bool {
    query_scrollable(
        &NoRoutines,
        &NoRoutines,
        query,
        &[],
        &crate::catalog::test_support::empty_scope(),
        requested,
    )
    .unwrap()
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
        assert_eq!(scrollable(&query(sql), None), expected, "{sql}");
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
        assert!(scrollable(&query, Some(true)), "{sql}");
        assert!(!scrollable(&query, Some(false)), "{sql}");
    }
}

#[test]
fn default_scroll_follows_outer_ordering_and_cte_materialization() {
    let mut mismatches = Vec::new();
    for (sql, expected) in [
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
        let actual = scrollable(&query(sql), None);
        if actual != expected {
            mismatches.push((sql, actual, expected));
        }
    }
    assert!(
        mismatches.is_empty(),
        "default scroll differences: {mismatches:?}"
    );
}

#[test]
fn inherited_deferred_sources_keep_their_query_traversal() {
    let mut scope = crate::catalog::test_support::empty_scope();
    for cte in query("WITH z AS (SELECT 1), a AS (SELECT * FROM z) SELECT * FROM a").ctes {
        scope.insert_deferred(cte);
    }
    for (sql, expected) in [
        ("SELECT * FROM a", false),
        ("SELECT * FROM public.a", true),
        ("WITH a AS MATERIALIZED (SELECT 1) SELECT * FROM a", true),
    ] {
        assert_eq!(
            query_scrollable(&NoRoutines, &NoRoutines, &query(sql), &[], &scope, None).unwrap(),
            expected,
            "{sql}"
        );
    }
}
