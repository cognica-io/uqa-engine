//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reject actual allocator requests on one test thread without changing the library's allocation paths.

#![allow(unsafe_code)]

use crate::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    ptr,
};

#[cfg(all(feature = "test-support", feature = "arrow-export"))]
#[path = "allocations/arrow.rs"]
mod arrow;
#[cfg(feature = "test-support")]
#[path = "allocations/callbacks.rs"]
mod callbacks;
#[cfg(feature = "test-support")]
use super::database as fixtures;
#[cfg(feature = "test-support")]
#[path = "allocations/handles.rs"]
mod handles;
#[cfg(feature = "test-support")]
#[path = "allocations/notifications.rs"]
mod notifications;
#[cfg(feature = "test-support")]
#[path = "allocations/target.rs"]
mod target;
#[cfg(feature = "test-support")]
#[path = "allocations/values.rs"]
mod values;

#[derive(Clone, Copy)]
struct Rejections {
    count: usize,
    remaining: Option<usize>,
}

thread_local! {
    static REJECTIONS: Cell<Option<Rejections>> = const { Cell::new(None) };
}

struct FaultAllocator;

fn reject() -> bool {
    REJECTIONS
        .try_with(|state| {
            let Some(mut rejection) = state.get() else {
                return false;
            };
            if rejection.remaining == Some(0) {
                return false;
            }
            rejection.count = rejection.count.saturating_add(1);
            rejection.remaining = rejection.remaining.map(|count| count - 1);
            state.set(Some(rejection));
            true
        })
        .unwrap_or(false)
}

// The allocator never unwinds. A rejected realloc preserves the original allocation, as GlobalAlloc requires; unrelated test threads keep the system allocator.
unsafe impl GlobalAlloc for FaultAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if reject() {
            ptr::null_mut()
        } else {
            System.alloc(layout)
        }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if reject() {
            ptr::null_mut()
        } else {
            System.alloc_zeroed(layout)
        }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if reject() {
            ptr::null_mut()
        } else {
            System.realloc(pointer, layout, size)
        }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout);
    }
}

#[global_allocator]
static ALLOCATOR: FaultAllocator = FaultAllocator;

fn rejecting_allocations<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            REJECTIONS.with(|state| state.set(None));
        }
    }
    REJECTIONS.with(|state| {
        assert!(state.get().is_none());
        state.set(Some(Rejections {
            count: 0,
            remaining: None,
        }));
    });
    let reset = Reset;
    let result = operation();
    let count =
        REJECTIONS.with(|state| state.get().expect("active allocator rejection scope").count);
    drop(reset);
    (result, count)
}

unsafe fn verify_constructor<T>(
    create: unsafe extern "C" fn(*mut *mut T) -> UQAStatus,
    destroy: unsafe extern "C" fn(*mut *mut T) -> UQAStatus,
) {
    // Initialize the diagnostic TLS before arming failure, as an embedding thread does on its first API call.
    let mut error = ptr::null();
    let _ = uqa_error_get_last(&raw mut error);
    let mut handle = ptr::null_mut();
    let mut status = UQA_OK;
    let mut completion = u32::MAX;
    let mut message = UQAStringView::default();
    let (observed, failures) = rejecting_allocations(|| {
        let result = create(&raw mut handle);
        let last = uqa_error_get_last(&raw mut error);
        let inspected = uqa_error_get_status(error, &raw mut status);
        let described = uqa_error_get_message(error, &raw mut message);
        let completed = uqa_error_get_completion(error, &raw mut completion);
        (result, last, inspected, described, completed)
    });
    assert_eq!(
        observed,
        (UQA_ERROR_OUT_OF_MEMORY, UQA_OK, UQA_OK, UQA_OK, UQA_OK)
    );
    assert_eq!(status, UQA_ERROR_OUT_OF_MEMORY);
    assert_eq!(completion, UQA_COMPLETION_NONE);
    assert_eq!(
        std::slice::from_raw_parts(message.data.cast::<u8>(), message.size),
        b"handle allocation failed"
    );
    assert_eq!(
        failures, 1,
        "diagnostic publication and inspection must allocate nothing"
    );
    assert!(handle.is_null());
    assert_eq!(destroy(&raw mut handle), UQA_OK);
    assert_eq!(create(&raw mut handle), UQA_OK);
    assert!(!handle.is_null());
    assert_eq!(destroy(&raw mut handle), UQA_OK);
    assert!(handle.is_null());
}

#[test]
fn checked_constructors_preserve_null_handles_and_allocation_free_diagnostics() {
    unsafe {
        verify_constructor(uqa_config_create, uqa_config_destroy);
        verify_constructor(uqa_result_create, uqa_result_destroy);
        verify_constructor(uqa_batch_result_create, uqa_batch_result_destroy);
        verify_constructor(uqa_buffer_create, uqa_buffer_destroy);
        verify_constructor(uqa_notification_create, uqa_notification_destroy);
        verify_constructor(uqa_udf_options_create, uqa_udf_options_destroy);
        verify_constructor(uqa_value_create_null, uqa_value_destroy);
        verify_constructor(uqa_value_create_void, uqa_value_destroy);
    }
}

#[test]
fn failed_string_replacement_preserves_the_original_and_recovers_after_allocator_failure() {
    unsafe {
        let mut config = ptr::null_mut();
        assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
        assert_eq!(
            uqa_config_set_encryption_key(config, c"original-secret".as_ptr()),
            UQA_OK
        );
        let mut original = UQAStringView::default();
        assert_eq!(
            uqa_config_get_encryption_key(config, &raw mut original),
            UQA_OK
        );
        for alias in [false, true] {
            let (status, failures) = rejecting_allocations(|| {
                if alias {
                    uqa_config_set_encryption_key_n(config, original.data, original.size)
                } else {
                    uqa_config_set_encryption_key(config, c"replacement-secret".as_ptr())
                }
            });
            assert_eq!(status, UQA_ERROR_OUT_OF_MEMORY);
            assert_eq!(failures, 1);
            let mut current = UQAStringView::default();
            assert_eq!(
                uqa_config_get_encryption_key(config, &raw mut current),
                UQA_OK
            );
            assert_eq!(current.data, original.data);
            assert_eq!(
                std::slice::from_raw_parts(current.data.cast::<u8>(), current.size),
                b"original-secret"
            );
        }
        assert_eq!(
            uqa_config_set_encryption_key(config, c"replacement-secret".as_ptr()),
            UQA_OK
        );
        assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
    }
}
