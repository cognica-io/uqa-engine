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
        let mut workspace = Workspace::bounded(memory.empty_reservation(), &original, &invoking);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            workspace.check(),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert!(matches!(
            workspace.vector::<u8>(1),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert!(matches!(
            workspace.decimal("1e400"),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert!(matches!(
            workspace.shape(&[Value::Int(1)], 1),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert!(matches!(
            workspace.retained(&Value::Int(1)),
            Err(ValueRetentionError::Cancelled(_))
        ));
        drop(workspace);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn tagged_conversion_retires_active_payloads_when_either_owner_cancels() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Barrier,
    };

    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(64 << 20);
        let map = tagged("bytes", [("hex", Value::Str("ff".repeat(8 << 20)))]);
        let input = retained_map(map, &memory);
        let before = memory.used();
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let ready = Barrier::new(2);
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                ready.wait();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while memory.peak() <= before
                    && !done.load(Ordering::Acquire)
                    && std::time::Instant::now() < deadline
                {
                    std::thread::yield_now();
                }
                assert!(memory.peak() > before, "no replacement allocation observed");
                assert!(
                    !done.load(Ordering::Acquire),
                    "conversion already completed"
                );
                if original_cancelled {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
            });
            ready.wait();
            let result = value_from_tagged_map_scoped(input, &original, &invoking);
            done.store(true, Ordering::Release);
            worker.join().unwrap();
            assert!(matches!(result, Err(ValueRetentionError::Cancelled(_))));
        });
        assert_eq!(memory.used(), 0);
    }
}
