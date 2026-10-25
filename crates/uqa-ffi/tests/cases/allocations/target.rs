//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select the actual allocator request without rejecting unrelated native cleanup allocations.

use super::{Rejections, REJECTIONS};
use std::cell::Cell;

thread_local! {
    static TARGET: Cell<Option<(&'static str, usize)>> = const { Cell::new(None) };
}

fn observe(name: &'static str) {
    TARGET.with(|target| {
        if let Some((expected, skip)) = target.get() {
            if name != expected {
                return;
            }
            if skip == 0 {
                target.set(None);
                REJECTIONS.with(|state| {
                    state.set(Some(Rejections {
                        count: 0,
                        remaining: Some(1),
                    }));
                });
            } else {
                target.set(Some((expected, skip - 1)));
            }
        }
    });
}

pub(super) fn reject<T, R>(operation: impl FnOnce() -> R) -> R {
    reject_named(std::any::type_name::<T>(), 0, operation)
}

pub(super) fn reject_named<R>(name: &'static str, skip: usize, operation: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TARGET.with(|target| target.set(None));
            REJECTIONS.with(|state| state.set(None));
        }
    }
    REJECTIONS.with(|state| assert!(state.get().is_none()));
    TARGET.with(|target| assert!(target.replace(Some((name, skip))).is_none()));
    let reset = Reset;
    let result = uqa_ffi::test_support::observe_handle_allocations(observe, operation);
    let reached = TARGET.with(|target| target.get().is_none());
    let rejected = REJECTIONS.with(|state| state.get().map(|state| state.count));
    drop(reset);
    assert!(
        reached,
        "handle allocation was not reached: {name}, ordinal {skip}"
    );
    assert_eq!(rejected, Some(1), "exactly one actual allocation must fail");
    result
}
