//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::MutableExecutablePlan;

fn stamp(query: &QueryPlan) -> (usize, i64) {
    let value = match &query.root {
        RelationalPlan::QueryBlock(block) => {
            match block.projections.first().map(|projection| &projection.expr) {
                Some(ScalarExpr::Literal(Value::Int(value))) => *value,
                _ => -1,
            }
        }
        _ => -2,
    };
    (query.ctes.len(), value)
}

#[test]
fn consuming_query_visitation_preserves_preorder_and_ownership_without_allocating() {
    for sql in [
        "WITH a AS (SELECT 1) SELECT (SELECT 2) FROM (SELECT 3) q",
        "EXPLAIN WITH a AS (SELECT 1) SELECT (SELECT 2) FROM (SELECT 3) q",
        "PREPARE p AS WITH a AS (SELECT 1) UPDATE t SET n = (SELECT 2) FROM (SELECT 3) q",
    ] {
        let mut source = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let mut expected = Vec::new();
        source.visit_queries_mut(&mut |query| expected.push(stamp(query)));
        let budget = MemoryBudget::new(4 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = source.copy_for_rewrite(&control).unwrap();
        let before = budget.used();
        let mut index = 0;
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(
                input
                    .map_queries_while(&control, &mut |query| {
                        assert_eq!(stamp(&query), expected[index]);
                        index += 1;
                        Ok((query, true))
                    })
                    .unwrap(),
            );
        });
        let output = output.unwrap();
        assert_eq!(index, expected.len());
        assert_eq!(allocations.count_total, 0);
        assert_eq!(budget.used(), before);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn stopped_query_root_retains_unvisited_children_and_callback_failure_releases_parent() {
    let source = UnifiedPlan::lower(
        crate::compile("SELECT (SELECT 1), (SELECT 2)")
            .unwrap()
            .remove(0),
    );
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let before = budget.used();
    let mut count = 0;
    let output = input
        .map_queries_while(&control, &mut |query| {
            count += 1;
            Ok((query, false))
        })
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(output.reserved_bytes(), before);
    assert!(matches!(output, MutableExecutablePlan::Query(_)));
    let error = output
        .map_queries_while(&control, &mut |query| {
            if stamp(&query).1 == 1 {
                return Err(SQLError::Internal("visitor failure".into()));
            }
            Ok((query, true))
        })
        .unwrap_err();
    assert_eq!(error.to_string(), "internal error: visitor failure");
    assert_eq!(budget.used(), 0);
}
