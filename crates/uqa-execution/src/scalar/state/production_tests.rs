//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn call_state_capacity_is_admitted_and_retained_through_duplicate_registration() {
    let expression = ScalarExpr::Column("value".into());
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let mut prepared = None;
    let allocation = allocation_counter::measure(|| {
        prepared = Some(
            PreparedExpressions::new_with_control(
                &expression,
                |expression, visit| {
                    visit(expression)?;
                    visit(expression)
                },
                &control,
            )
            .unwrap(),
        );
    });
    let prepared = prepared.unwrap();
    assert_eq!(prepared.calls().columns.len(), 1);
    assert!(prepared.calls().columns.capacity() >= 2);
    assert_eq!(allocation.bytes_current as usize, budget.used());
    drop(prepared);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_call_state_construction_stops_root_traversal_and_releases_private_buffers() {
    let expressions = (0..256)
        .map(|i| ScalarExpr::Column(format!("column_{i}").into()))
        .collect::<Vec<_>>();
    let visits = std::cell::Cell::new(0);
    let roots = (expressions.as_slice(), &visits);
    let visit_roots: VisitRoots<(&[ScalarExpr], &std::cell::Cell<usize>)> =
        |(expressions, visits), visit| {
            expressions.iter().try_for_each(|expression| {
                visits.set(visits.get() + 1);
                visit(expression)
            })
        };
    let budget = MemoryBudget::new(256);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let error = PreparedExpressions::new_with_control(roots, visit_roots, &control)
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(visits.get() > 0 && visits.get() < expressions.len());
    assert_eq!(budget.used(), 0);
    for token in [&original, &invoking] {
        visits.set(0);
        token.cancel();
        let error = PreparedExpressions::new_with_control(roots, visit_roots, &control)
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(visits.get(), 0);
        assert_eq!(budget.used(), 0);
        token.reset();
    }
}
