//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[test]
fn routine_authority_input_retires_every_cancelled_and_abandoned_boundary() {
    let (source, roles) = source(4, 0);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let polls = finish(
        &mut input(source.clone(), roles.clone(), &memory, &token, &token),
        1,
    )
    .unwrap();
    assert!(polls > 32);
    for boundary in 0..polls {
        for terminal in ["original", "invoking", "zero", "drop"] {
            let memory = MemoryBudget::new(1 << 20);
            let sibling = memory.reserve(17).unwrap();
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = input(source.clone(), roles.clone(), &memory, &original, &invoking);
            for _ in 0..boundary {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            match terminal {
                "original" => original.cancel(),
                "invoking" => invoking.cancel(),
                _ => (),
            }
            if terminal != "drop" {
                let error = advance(&mut input, usize::from(terminal != "zero")).unwrap_err();
                if terminal != "zero" {
                    assert_eq!(error.sqlstate(), Some("57014"));
                }
                assert!(advance(&mut input, 1).is_err());
            }
            drop(input);
            assert_eq!(memory.used(), sibling.bytes(), "{boundary}: {terminal}");
        }
    }
}

#[test]
fn routine_authority_input_preserves_admission_failure_and_unwind_retirement() {
    let (source, roles) = source(12, 0);
    for terminal in ["memory", "unwind"] {
        let memory = MemoryBudget::new(1 << 20);
        let sibling = memory.reserve(17).unwrap();
        let token = CancellationToken::new();
        let mut input = input(source.clone(), roles.clone(), &memory, &token, &token);
        assert!(advance(&mut input, 16).unwrap().is_none());
        if terminal == "memory" {
            let occupied = memory.reserve(memory.available()).unwrap();
            let error = finish(&mut input, 1).unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"));
            assert!(advance(&mut input, 1).is_err());
            drop((input, occupied));
        } else {
            let result = catch_unwind(AssertUnwindSafe(move || {
                let _owner = input;
                panic!("retire routine authority validation");
            }));
            assert!(result.is_err());
        }
        assert_eq!(memory.used(), sibling.bytes());
    }
}
