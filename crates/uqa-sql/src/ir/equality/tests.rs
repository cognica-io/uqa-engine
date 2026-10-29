//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{RelationalPlan, UnifiedPlan};
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_syntax_equality_preserves_exact_metadata_and_child_order() {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile("SELECT a, q.a, 1, '1', 1::int, ARRAY[1,2], ROW(1,2), CASE WHEN a>1 THEN 2 ELSE 3 END, coalesce(a,1), sum(a) OVER (PARTITION BY b ORDER BY c ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)").unwrap().remove(0)) else { unreachable!() };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    let budget = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let allocation = allocation_counter::measure(|| {
        for left in &block.projections {
            for right in &block.projections {
                assert_eq!(
                    left.expr
                        .has_same_syntax_with_control(&right.expr, &control)
                        .unwrap(),
                    left.expr == right.expr
                );
            }
        }
    });
    assert_eq!(allocation.count_total, 0);
    invoking.cancel();
    assert_eq!(
        block.projections[0]
            .expr
            .has_same_syntax_with_control(&block.projections[0].expr, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
}
