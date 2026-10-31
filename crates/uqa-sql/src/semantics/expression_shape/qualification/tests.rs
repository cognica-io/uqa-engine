//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn owned_qualification_preserves_separate_ordering_scopes_and_exact_lifetimes() {
    let compiled = crate::plan::UnifiedPlan::lower(crate::compile("SELECT sum(x ORDER BY y) FILTER (WHERE z>0), sum(x) FILTER (WHERE z>0) OVER(PARTITION BY p ORDER BY q) FROM t").unwrap().remove(0));
    let crate::plan::UnifiedPlan::Query(query) = compiled else {
        panic!("query")
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = &query.root else {
        panic!("block")
    };
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for projection in &block.projections {
        let expected = crate::semantics::qualify_unqualified_columns(&projection.expr, "source");
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = Some(
                qualify_unqualified_columns_with_control(&projection.expr, "source", &control)
                    .unwrap(),
            );
        });
        assert_eq!(output.as_deref(), Some(&expected));
        assert_eq!(allocation.bytes_current, budget.used() as i64);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    token.cancel();
    assert_eq!(
        qualify_unqualified_columns_with_control(&block.projections[0].expr, "source", &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), 0);
}
