//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn tagged_conversion_suboperations_check_both_cancellation_owners() {
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut workspace = Workspace::new(
            Some(memory.empty_reservation()),
            ProductionControl::new(&memory, &original, &invoking),
        );
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            inline(workspace.checkpoint(&mut ())),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert!(matches!(
            workspace.vector::<u8>(1),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert!(matches!(
            inline(workspace.decimal("1e400", &mut ())),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert!(matches!(
            inline(workspace.shape(&[Value::Int(1)], 1, &mut ())),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert!(matches!(
            inline(workspace.retained(&Value::Int(1), &mut ())),
            Err(ValueRetentionError::Cancelled(_))
        ));
        drop(workspace);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn tagged_conversion_retires_active_payloads_when_either_owner_cancels() {
    use std::{cell::Cell, rc::Rc};

    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(64 << 10);
        let map = tagged("bytes", [("hex", Value::Str("ff".repeat(4096)))]);
        let input = retained_map(map, &memory);
        let before = memory.used();
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let observed = Rc::new(Cell::new(false));
        let seen = Rc::clone(&observed);
        let allowance = memory.clone();
        let cancellation = if original_cancelled {
            original.clone()
        } else {
            invoking.clone()
        };
        super::super::allocation::AFTER_VECTOR_ALLOCATION.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                assert!(
                    allowance.used() > before,
                    "replacement payload must be live"
                );
                seen.set(true);
                cancellation.cancel();
            }));
        });
        let result = value_from_tagged_map_scoped(input, &original, &invoking);
        assert!(
            observed.get(),
            "conversion must reach the replacement allocation"
        );
        assert!(matches!(result, Err(ValueRetentionError::Cancelled(_))));
        assert_eq!(memory.used(), 0);
    }
}
