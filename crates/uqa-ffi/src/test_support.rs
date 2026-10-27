//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rust-only access for native fault qualification behind the real C boundary.

#![allow(unsafe_code)]

use crate::{boundary, error, UQAEngine, UQAStatus};
use std::cell::Cell;

type AllocationObserver = fn(&'static str);

thread_local! {
    static ALLOCATION_OBSERVER: Cell<Option<AllocationObserver>> = const { Cell::new(None) };
}

/// Observe checked handle allocations on this thread immediately before the actual allocator call.
///
/// A test allocator can reject that request while permitting allocations needed by native cleanup. The observer must not allocate or unwind. Nested scopes restore their caller's observer, including on unwind.
pub fn observe_handle_allocations<T>(
    observer: AllocationObserver,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<AllocationObserver>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ALLOCATION_OBSERVER.with(|observer| observer.set(self.0));
        }
    }
    let restore = Restore(ALLOCATION_OBSERVER.with(|slot| slot.replace(Some(observer))));
    let result = operation();
    drop(restore);
    result
}

pub(crate) fn before_handle_allocation<T>() {
    ALLOCATION_OBSERVER.with(|observer| {
        if let Some(observer) = observer.get() {
            observer(std::any::type_name::<T>());
        }
    });
}

/// Read the live FFI allowance while holding the ordinary session call lease.
///
/// # Safety
/// `engine` must obey the live-handle and synchronization requirements of an ordinary C operation.
pub unsafe fn retained_ffi_bytes(engine: *mut UQAEngine) -> Result<usize, UQAStatus> {
    let session = &boundary::input(engine).map_err(error::publish)?.session;
    let _call = session.enter().map_err(error::publish)?;
    Ok(session.memory.used())
}

/// Use the actual native session while holding its ordinary exclusive call lease.
///
/// # Safety
/// `engine` must be a live C handle. The caller must obey the same lifetime and synchronization requirements as an ordinary C operation.
pub unsafe fn with_native_engine<T>(
    engine: *mut UQAEngine,
    operation: impl FnOnce(&uqa_engine::Engine) -> T,
) -> Result<T, UQAStatus> {
    let session = &boundary::input(engine).map_err(error::publish)?.session;
    let _call = session.enter().map_err(error::publish)?;
    Ok(operation(&session.native))
}
