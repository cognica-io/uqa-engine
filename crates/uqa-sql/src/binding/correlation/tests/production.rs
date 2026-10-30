//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn query(sql: &str) -> QueryPlan {
    let crate::plan::UnifiedPlan::Query(query) =
        crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *query
}

#[test]
fn controlled_correlation_borrows_function_arguments_and_releases_namespaces() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = CorrelationContext {
        routines: &crate::binding::fixture::NoRoutines,
        catalog: binding.catalog.as_ref(),
        resolution: &binding.resolution,
        scoped_relations: None,
    };
    let wide = "x".repeat(4 << 20);
    let plan = query(&format!("SELECT value FROM ROWS FROM (json_each('{{\"x\":\"{wide}\"}}'), generate_series(1,2)) AS items(key,value,n)"));
    let budget = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let measured = allocation_counter::measure(|| {
        assert!(!query_depends_on_outer_row_with_control(context, &plan, &control).unwrap());
    });
    assert!(measured.bytes_total < 64 << 10, "{measured:?}");
    assert_eq!(measured.bytes_current, 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_correlation_rejects_wide_names_before_copying_and_recovers() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = CorrelationContext {
        routines: &crate::binding::fixture::NoRoutines,
        catalog: binding.catalog.as_ref(),
        resolution: &binding.resolution,
        scoped_relations: None,
    };
    let mut plan = query("SELECT local.x FROM (VALUES(1)) local(x)");
    let RelationalPlan::QueryBlock(block) = &mut plan.root else {
        panic!("block")
    };
    let Some(
        SourcePlan::Subquery { column_aliases, .. } | SourcePlan::Values { column_aliases, .. },
    ) = &mut block.from
    else {
        panic!("source")
    };
    column_aliases[0] = "x".repeat(4 << 20);
    let recovery = query("WITH a(x) AS (VALUES(1)), b(y) AS (SELECT x FROM a) SELECT y FROM b");
    for limit in [0, 128, 1024, 8192, 64 << 10] {
        let parent = MemoryBudget::new(limit + 37);
        let sibling = parent.reserve(37).unwrap();
        let budget = parent.child(limit);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                query_depends_on_outer_row_with_control(context, &plan, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
        });
        assert!(measured.bytes_total < 64 << 10, "{measured:?}");
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
        if limit >= 64 << 10 {
            assert!(
                !query_depends_on_outer_row_with_control(context, &recovery, &control).unwrap()
            );
            assert_eq!(parent.used(), sibling.bytes());
        }
    }
    for request in [false, true] {
        let budget = MemoryBudget::new(64 << 10);
        let original = CancellationToken::new();
        let session = CancellationToken::new();
        if request {
            original.cancel();
        } else {
            session.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &session);
        assert_eq!(
            query_depends_on_outer_row_with_control(context, &plan, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn controlled_correlation_preserves_cte_join_window_and_lateral_scopes() {
    let binding = crate::binding::fixture::empty_binding_context();
    let context = CorrelationContext {
        routines: &crate::binding::fixture::NoRoutines,
        catalog: binding.catalog.as_ref(),
        resolution: &binding.resolution,
        scoped_relations: None,
    };
    for (sql, expected) in [
        ("WITH a(x) AS (VALUES(1)), b AS (SELECT * FROM a) SELECT x FROM b", false),
        ("WITH RECURSIVE a(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM a WHERE x<3) SELECT x FROM a", false),
        ("SELECT x FROM (VALUES(1)) a(x) NATURAL JOIN (VALUES(1)) b(x)", false),
        ("SELECT j.x FROM (VALUES(1)) a(x) JOIN (VALUES(1)) b(x) USING(x) AS j", false),
        ("SELECT sum(x) OVER (ORDER BY x ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM (VALUES(1)) a(x)", false),
        ("SELECT sum(x) OVER (ORDER BY x ROWS BETWEEN parent.n PRECEDING AND CURRENT ROW) FROM (VALUES(1)) a(x)", true),
        ("SELECT n FROM (VALUES(1)) a(x), LATERAL generate_series(1,a.x) b(n)", false),
        ("SELECT n FROM (VALUES(1)) a(x), LATERAL generate_series(1,parent.x) b(n)", true),
    ] {
        let plan = query(sql);
        let budget = MemoryBudget::new(256 << 10);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        assert_eq!(query_depends_on_outer_row_with_control(context, &plan, &control).unwrap(), expected, "{sql}");
        assert_eq!(budget.used(), 0, "{sql}");
    }
}
