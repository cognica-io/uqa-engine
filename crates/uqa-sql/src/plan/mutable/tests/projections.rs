//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fixture, MemoryBudget, ProductionControl, RelationalPlan};
use uqa_core::CancellationToken;

#[test]
fn selected_projection_keeps_names_and_scalar_owners_while_discarded_slots_retire() {
    let source = fixture();
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let RelationalPlan::QueryBlock(block) = &input.root else {
        unreachable!()
    };
    let alias = block.projections[1].alias.as_ref().unwrap().as_ptr();
    let before = budget.used();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        let (query, mut projections) = input.take_projections(&control).unwrap();
        drop(projections.next().unwrap());
        let selected = projections.next().unwrap();
        drop(projections);
        output = Some(
            query
                .replace_projections(std::iter::once(selected), &control)
                .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(
        before as i64 + allocations.bytes_current,
        budget.used() as i64
    );
    assert_eq!(output.reserved_bytes(), budget.used());
    let RelationalPlan::QueryBlock(block) = &output.root else {
        unreachable!()
    };
    assert_eq!(block.projections.len(), 1);
    assert_eq!(block.projections[0].alias.as_ref().unwrap().as_ptr(), alias);
    let output = output
        .replace_projection_alias(0, Some(control.copy_text("renamed").unwrap()), &control)
        .unwrap();
    assert_eq!(output.reserved_bytes(), budget.used());
    drop(output.finish(&control).unwrap());
    assert_eq!(budget.used(), 0);
}

#[test]
fn replacement_failure_releases_parent_container_and_each_unvisited_projection() {
    let source = fixture();
    for mode in 0..3 {
        let budget = MemoryBudget::new(4 * 1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = source.copy_for_rewrite(&control).unwrap();
        let (query, projections) = input.take_projections(&control).unwrap();
        let sibling = budget.reserve(budget.available()).unwrap();
        if mode == 1 {
            original.cancel();
        }
        if mode == 2 {
            invoking.cancel();
        }
        let error = query
            .replace_projections(projections, &control)
            .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if mode == 0 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), sibling.bytes());
    }
}

#[test]
fn projection_iterator_unwind_retires_transferred_and_unvisited_items() {
    let source = fixture();
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let (query, projections) = input.take_projections(&control).unwrap();
    let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let projections = projections.enumerate().map(|(index, projection)| {
            assert_eq!(index, 0, "injected projection iterator unwind");
            projection
        });
        let _ = query.replace_projections(projections, &control);
    }));
    assert!(error.is_err());
    assert_eq!(budget.used(), 0);
}
