//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_builtin_authority_preserves_selected_identity_and_denial_diagnostics() {
    let permissions = super::super::tests::permissions();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for (name, types) in [
        ("pg_catalog.lower", &["text"][..]),
        ("\"pg_catalog\".\"lower\"", &["text"][..]),
        ("sum", &["integer"][..]),
        ("lower", &["integer"][..]),
        ("upper", &["text"][..]),
        ("public.lower", &["text"][..]),
    ] {
        let binding = super::super::tests::binding(name, types);
        let expected = permissions
            .require_execute(&binding)
            .map_err(|error| error.to_string());
        let actual = permissions
            .require_with_control(&binding, &control)
            .map_err(|error| error.to_string());
        assert_eq!(actual, expected, "{name}");
        assert_eq!(memory.used(), 0);
    }
    let binding = super::super::tests::binding("lower", &["text"]);
    let zero = MemoryBudget::new(0);
    assert_eq!(
        permissions
            .require_with_control(&binding, &ProductionControl::new(&zero, &token, &token))
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(zero.used(), 0);
}

#[test]
fn controlled_builtin_authority_borrows_nonselected_names_and_stops_on_either_cancellation() {
    let permissions = super::super::tests::permissions();
    let binding = super::super::tests::binding(&"x".repeat(1 << 20), &[]);
    // Initialize immutable catalog metadata independently of this lookup's work.
    permissions
        .require_execute(&super::super::tests::binding("upper", &["text"]))
        .unwrap();
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let allocations = allocation_counter::measure(|| {
        permissions
            .require_with_control(&binding, &control)
            .unwrap();
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(memory.used(), 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            permissions
                .require_with_control(
                    &binding,
                    &ProductionControl::new(&memory, &original, &invoking)
                )
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
    }
}
