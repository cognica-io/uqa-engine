//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic cancellation at a parser boundary with real admitted native storage.

use std::cell::RefCell;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

struct Cancellation {
    memory: MemoryBudget,
    token: CancellationToken,
    minimum: usize,
    fired: bool,
}

thread_local! {
    static ACTIVE: RefCell<Option<Cancellation>> = const { RefCell::new(None) };
}

pub(crate) fn observe(control: &ProductionControl<'_>) {
    ACTIVE.with_borrow_mut(|active| {
        if let Some(active) = active {
            if !active.fired
                && control
                    .budget()
                    .is_some_and(|memory| memory.shares_allowance(&active.memory))
                && active.memory.used() >= active.minimum
            {
                active.fired = true;
                active.token.cancel();
            }
        }
    });
}

/// Cancel only after the selected producer owns real native allocations. Thread-local state is removed on normal return or unwinding; no scheduler timing determines whether the cancellation was exercised.
pub(crate) fn cancel_after_allocation<T>(
    memory: &MemoryBudget,
    token: &CancellationToken,
    minimum: usize,
    run: impl FnOnce() -> T,
) -> T {
    ACTIVE.with_borrow_mut(|active| {
        assert!(active.is_none(), "nested stored-input cancellation fixture");
        *active = Some(Cancellation {
            memory: memory.clone(),
            token: token.clone(),
            minimum,
            fired: false,
        });
    });
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ACTIVE.with_borrow_mut(|active| *active = None);
        }
    }
    let _reset = Reset;
    let output = run();
    ACTIVE.with_borrow(|active| {
        assert!(
            active.as_ref().unwrap().fired,
            "no active native allocation observed"
        );
    });
    output
}
