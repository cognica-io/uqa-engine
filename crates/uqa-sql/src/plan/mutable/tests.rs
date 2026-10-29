//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::UnifiedPlan;
use uqa_core::{memory::MemoryBudget, CancellationToken};

mod children;
mod traversal;
mod vectors;

fn fixture() -> QueryPlan {
    let UnifiedPlan::Query(mut query) =
        UnifiedPlan::lower(crate::compile("SELECT 1, 2").unwrap().remove(0))
    else {
        panic!("query fixture");
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("query block");
    };
    block.projections[0].expr = ScalarExpr::Array(vec![
        ScalarExpr::Literal(Value::Str("discarded".repeat(4096))),
        ScalarExpr::Literal(Value::Str("selected".repeat(4096))),
    ]);
    block.projections[1].alias = Some("retired_alias".repeat(2048));
    *query
}

#[test]
fn mutable_query_copy_and_edits_preserve_exact_live_constructor_ownership() {
    let source = fixture();
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = None;
    let copied = allocation_counter::measure(|| {
        output = Some(source.copy_for_rewrite(&control).unwrap());
    });
    let mut output = output.unwrap();
    assert_eq!(copied.bytes_current as usize, output.reserved_bytes());
    assert_eq!(budget.used(), output.reserved_bytes());
    let before = budget.used();
    let mut changed = None;
    let selected = allocation_counter::measure(|| {
        changed = Some(
            output
                .map_scalar(QueryScalarSlot::Projection(0), &control, |scalar| {
                    Ok(scalar.select_child(1))
                })
                .unwrap(),
        );
    });
    output = changed.take().unwrap();
    assert_eq!(selected.count_total, 0);
    assert_eq!(before as i64 + selected.bytes_current, budget.used() as i64);
    let before = budget.used();
    let retired = allocation_counter::measure(|| {
        changed = Some(
            output
                .retain_projections(&control, |index, _| Ok(index == 0))
                .unwrap(),
        );
    });
    output = changed.take().unwrap();
    assert_eq!(retired.count_total, 0);
    assert_eq!(before as i64 + retired.bytes_current, budget.used() as i64);
    let before = budget.used();
    let mut frozen = None;
    let finished = allocation_counter::measure(|| {
        frozen = Some(output.finish(&control).unwrap());
    });
    let frozen = frozen.unwrap();
    assert_eq!(finished.count_total, 0);
    assert_eq!(before as i64 + finished.bytes_current, budget.used() as i64);
    assert_eq!(frozen.reserved_bytes(), budget.used());
    let RelationalPlan::QueryBlock(block) = &frozen.root else {
        panic!("query block");
    };
    assert_eq!(block.projections.len(), 1);
    assert!(
        matches!(&block.projections[0].expr, ScalarExpr::Literal(Value::Str(value)) if value == &"selected".repeat(4096))
    );
    drop(frozen);
    assert_eq!(budget.used(), 0);
}

#[test]
fn mutable_query_copy_rejects_before_large_payload_growth_and_cleans_partial_records() {
    let source = fixture();
    let budget = MemoryBudget::new(2048);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            source.copy_for_rewrite(&control).unwrap_err().sqlstate(),
            Some("53200")
        );
    });
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    assert_eq!(budget.used(), 0);
    let RelationalPlan::QueryBlock(block) = &source.root else {
        panic!("query block");
    };
    assert_eq!(block.projections.len(), 2);
}

#[test]
fn mutable_query_callback_failure_cancellation_and_unwind_retire_all_private_fields() {
    let source = fixture();
    for failure in 0..4 {
        let budget = MemoryBudget::new(4 * 1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let query = source.copy_for_rewrite(&control).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            query.map_scalar(QueryScalarSlot::Projection(0), &control, |scalar| {
                match failure {
                    0 => return Err(SQLError::Internal("callback failure".into())),
                    1 => original.cancel(),
                    2 => invoking.cancel(),
                    _ => panic!("callback unwind"),
                }
                Ok(scalar.select_child(0))
            })
        }));
        if failure == 3 {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_err());
        }
        assert_eq!(budget.used(), 0, "failure {failure}");
    }
}
