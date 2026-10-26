//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::needed;
use crate::catalog::test_support::empty_scope;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};
use uqa_sql::plan::{QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *query
}

#[test]
fn named_input_inspection_preserves_lexical_and_prepared_source_selection() {
    let mut scope = empty_scope();
    for (sql, expected) in [
        ("SELECT 1", false),
        ("WITH q AS (VALUES (1)) SELECT 1", false),
        ("WITH q AS (VALUES (1)) SELECT * FROM q", true),
        ("WITH q AS (VALUES (1)) SELECT * FROM public.q", false),
        ("WITH q AS (VALUES (1)) SELECT * FROM (SELECT * FROM q) nested", true),
        ("WITH q AS (VALUES (1)), r AS (SELECT * FROM q) SELECT 1", true),
        ("WITH q AS (VALUES (1)) SELECT 1 UNION ALL SELECT * FROM q", true),
        ("WITH q AS (VALUES (1)) SELECT (SELECT * FROM q)", true),
        ("WITH q AS (VALUES (1)) SELECT * FROM (WITH q AS (SELECT * FROM q) SELECT * FROM q) nested", true),
        ("WITH written AS (DELETE FROM t RETURNING a) SELECT 1", false),
        ("WITH written AS (DELETE FROM t RETURNING a) SELECT * FROM written", true),
        (r#"WITH "Q.CTE" AS (VALUES (1)) SELECT * FROM "Q.CTE""#, true),
        (r#"WITH "Q.""CTE" AS (VALUES (1)) SELECT * FROM "Q.""CTE""#, true),
    ] {
        assert_eq!(needed(&query(sql), &scope, ProductionControl::uncontrolled()).unwrap(), expected, "{sql}");
    }
    let mut prepared = query("WITH q AS (VALUES (1)) SELECT * FROM q");
    let RelationalPlan::QueryBlock(block) = &mut prepared.root else {
        panic!("block")
    };
    let Some(SourcePlan::Table {
        executable_query, ..
    }) = &mut block.from
    else {
        panic!("table")
    };
    *executable_query = Some(Box::new(query("VALUES (1)")));
    assert!(!needed(&prepared, &scope, ProductionControl::uncontrolled()).unwrap());

    let names = ["q"];
    {
        let visible = scope.enter_visible_ctes(names);
        assert!(!needed(
            &query("SELECT * FROM q"),
            &visible,
            ProductionControl::uncontrolled()
        )
        .unwrap());
    }
    scope.insert_deferred(
        query("WITH q AS (VALUES (1)) SELECT 1")
            .ctes
            .remove(0)
            .into(),
    );
    assert!(needed(
        &query("SELECT * FROM q"),
        &scope,
        ProductionControl::uncontrolled()
    )
    .unwrap());
    assert!(!needed(
        &query("SELECT * FROM public.q"),
        &scope,
        ProductionControl::uncontrolled()
    )
    .unwrap());
}

#[test]
fn inspection_admits_only_borrowed_names_and_never_copies_cte_payloads() {
    let scope = empty_scope();
    let token = CancellationToken::new();
    let mut baseline = None;
    for size in [1, 4 * 1024 * 1024] {
        let mut plan = query("WITH q AS (SELECT 'value' AS value) SELECT value FROM q");
        let RelationalPlan::QueryBlock(block) = &mut plan.ctes[0].body.query_mut().unwrap().root
        else {
            panic!("block")
        };
        block.projections[0].expr = uqa_sql::ScalarExpr::Literal(Value::Str("x".repeat(size)));
        let budget = MemoryBudget::new(1024);
        let allocation = allocation_counter::measure(|| {
            assert!(needed(
                &plan,
                &scope,
                ProductionControl::new(&budget, &token, &token)
            )
            .unwrap());
        });
        assert!(allocation.bytes_total < 1024, "{allocation:?}");
        assert_eq!(budget.used(), 0);
        assert!(budget.peak() > 0 && budget.peak() < 1024);
        if let Some(bytes) = baseline {
            assert_eq!(allocation.bytes_total, bytes);
        }
        baseline = Some(allocation.bytes_total);
        let rejected = MemoryBudget::new(0);
        assert!(needed(
            &plan,
            &scope,
            ProductionControl::new(&rejected, &token, &token)
        )
        .is_err());
        assert_eq!(rejected.used(), 0);
        for cancel_original in [false, true] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
            assert!(needed(
                &plan,
                &scope,
                ProductionControl::new(&budget, &original, &invoking)
            )
            .is_err());
            assert_eq!(budget.used(), 0);
        }
    }
}
