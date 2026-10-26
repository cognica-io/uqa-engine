//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::retained_expressions;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};
use uqa_sql::{
    plan::{CommandPlan, SourcePlan, UnifiedPlan},
    ScalarExpr,
};

#[test]
fn mutation_expression_arrays_retain_their_own_capacity_without_copying_payloads() {
    let UnifiedPlan::Command(command) = UnifiedPlan::lower(
        uqa_sql::compile(
            "UPDATE items SET n = $1 FROM input_fn($2, $3) src WHERE n > $4 RETURNING $5",
        )
        .unwrap()
        .remove(0),
    ) else {
        panic!("command");
    };
    let CommandPlan::Update(mut update) = *command else {
        panic!("update");
    };
    update.assignments[0].value = ScalarExpr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)));
    let SourcePlan::Function { args, .. } = update.source.as_deref().unwrap() else {
        panic!("source function");
    };
    let expected = [
        &update.assignments[0].value,
        update.predicate.as_ref().unwrap(),
        &update.returning[0].expr,
        &args[0],
        &args[1],
    ];
    let parent = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let mut rejected = 0;
    let mut accepted = 0;
    for limit in [0, 8, 16, 24, 32, 40, 64, 128, 256] {
        let memory = parent.child(limit);
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(retained_expressions(
                ProductionControl::new(&memory, &original, &invoking),
                update.expressions(),
                update.source.as_deref(),
            ));
        });
        match output.unwrap() {
            Ok(retained) => {
                accepted += 1;
                assert_eq!(retained.len(), expected.len());
                for (actual, expected) in retained.iter().zip(expected) {
                    assert!(std::ptr::eq(*actual, expected));
                }
                assert_eq!(
                    retained.reserved_bytes(),
                    retained.capacity() * size_of::<&ScalarExpr>()
                );
                assert_eq!(memory.used(), retained.reserved_bytes());
                assert_eq!(parent.used(), memory.used());
                assert!(allocations.bytes_total < 1024, "{allocations:?}");
                drop(retained);
            }
            Err(error) => {
                rejected += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
            }
        }
        assert_eq!(memory.used(), 0);
        assert_eq!(parent.used(), 0);
    }
    assert!(accepted > 0 && rejected > 0);
    for token in [&original, &invoking] {
        token.cancel();
        let error = retained_expressions(
            ProductionControl::new(&parent, &original, &invoking),
            update.expressions(),
            update.source.as_deref(),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(parent.used(), 0);
        token.reset();
    }
}
