//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn normalized_names_borrow_and_folded_names_own_exact_admitted_storage() {
    for (name, expected, borrowed) in [
        ("pg_catalog.length", "length", true),
        ("PG_CATALOG.LENGTH", "length", false),
        ("PUBLIC.ÉCLAIR", "public.Éclair", false),
        ("public.Éclair", "public.Éclair", true),
        ("AG_CATALOG.CYPHER", "cypher", false),
    ] {
        assert_eq!(super::super::builtin_function_dispatch_name(name), expected);
        let budget = MemoryBudget::new(1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = Some(builtin_function_dispatch_name_with_control(name, &control).unwrap());
        });
        let output = output.unwrap();
        assert_eq!(output.as_str(), expected);
        assert_eq!(matches!(output, FunctionName::Borrowed(_)), borrowed);
        assert_eq!(allocation.bytes_current as usize, output.reserved_bytes());
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn name_rejection_precedes_case_folding_and_both_cancellations_are_allocation_free() {
    let name = "LONG_NAME".repeat(16384);
    let budget = MemoryBudget::new(1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let allocation = allocation_counter::measure(|| {
        assert!(matches!(
            builtin_function_dispatch_name_with_control(&name, &control),
            Err(ValueRetentionError::Memory(_))
        ));
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(budget.used(), 0);
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(control.check().is_err());
        let allocation = allocation_counter::measure(|| {
            assert!(matches!(
                builtin_function_dispatch_name_with_control(&name, &control),
                Err(ValueRetentionError::Cancelled(_))
            ));
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(budget.used(), 0);
    }
}
