//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native Rust callbacks unwind inside the library; no panic crosses an extern C callback.

#![allow(unsafe_code)]

use crate::*;
use std::{
    ffi::{c_void, CString},
    ptr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use uqa_core::Value;

struct Released(Arc<AtomicUsize>);
impl Drop for Released {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

unsafe extern "C" fn consume(
    _: *mut c_void,
    _: *const u8,
    count: usize,
    written: *mut usize,
) -> UQAStatus {
    written.write(count);
    UQA_OK
}

#[derive(Clone, Copy, Debug)]
enum Operation {
    Message,
    Prepared,
    Cursor,
    Copy,
}

unsafe fn register_native_panic(engine: *mut UQAEngine, releases: &Arc<AtomicUsize>) {
    let probe = Released(Arc::clone(releases));
    test_support::with_native_engine(engine, |native| {
        native
            .register_scalar_function(
                "ffi_native_panic",
                move |_: &[Value]| -> Result<Value, uqa_sql::SQLError> {
                    let _keep_capture = &probe;
                    std::panic::panic_any("injected native execution panic");
                },
            )
            .unwrap();
    })
    .unwrap();
}

unsafe fn verify_panic_diagnostic() {
    let mut error = ptr::null();
    let mut completion = UQA_COMPLETION_NONE;
    let mut message = UQAStringView::default();
    assert_eq!(uqa_error_get_last(&raw mut error), UQA_OK);
    assert_eq!(uqa_error_get_completion(error, &raw mut completion), UQA_OK);
    assert_eq!(completion, UQA_COMPLETION_UNKNOWN);
    assert_eq!(uqa_error_get_message(error, &raw mut message), UQA_OK);
    assert_eq!(
        std::slice::from_raw_parts(message.data.cast::<u8>(), message.size),
        b"panic at the C ABI boundary"
    );
}

unsafe fn verify_reopen(path: *const std::ffi::c_char, flags: UQAOpenFlags) {
    let mut engine = ptr::null_mut();
    let mut result = ptr::null_mut();
    if flags != UQA_OPEN_MEMORY {
        assert_eq!(
            uqa_engine_open(path, flags, ptr::null_mut(), &raw mut engine),
            UQA_OK
        );
        assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
        assert_eq!(
            uqa_engine_execute(
                engine,
                c"SELECT count(*) FROM panic_effects".as_ptr(),
                result
            ),
            UQA_OK
        );
        let mut value = ptr::null();
        let mut count = -1;
        assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut value), UQA_OK);
        assert_eq!(uqa_value_get_int64(value, &raw mut count), UQA_OK);
        assert_eq!(count, 0, "panic effects survived close/reopen");
        assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
    }
}

unsafe fn open_subscription(engine: *mut UQAEngine) -> *mut UQASubscription {
    let mut subscription = ptr::null_mut();
    let channels = [c"panic_channel".as_ptr()];
    let subscription_options = UQASubscriptionOptions {
        struct_size: size_of::<UQASubscriptionOptions>() as u32,
        version: 1,
        max_active_subscriptions: 1,
        max_channels: 1,
        max_queued_notifications: 8,
        max_queued_bytes: 4096,
        max_registry_entries_per_poll: 8,
    };
    assert_eq!(
        uqa_subscription_open(
            engine,
            channels.as_ptr(),
            channels.len(),
            &raw const subscription_options,
            &raw mut subscription
        ),
        UQA_OK
    );
    subscription
}

unsafe fn verify(flags: UQAOpenFlags, operation: Operation, transaction: bool) {
    let directory = tempfile::tempdir().unwrap();
    let path = CString::new(directory.path().join("panic.db").to_str().unwrap()).unwrap();
    let path = if flags == UQA_OPEN_MEMORY {
        ptr::null()
    } else {
        path.as_ptr()
    };
    let mut engine = ptr::null_mut();
    assert_eq!(
        uqa_engine_open(path, flags, ptr::null_mut(), &raw mut engine),
        UQA_OK
    );
    let releases = Arc::new(AtomicUsize::new(0));
    register_native_panic(engine, &releases);
    let mut result = ptr::null_mut();
    let mut statement = ptr::null_mut();
    let mut cursor = ptr::null_mut();
    let mut failed_cursor = ptr::null_mut();
    let mut notification = ptr::null_mut();
    assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
    assert_eq!(uqa_notification_create(&raw mut notification), UQA_OK);
    assert_eq!(
        uqa_engine_execute(
            engine,
            c"CREATE TABLE panic_effects (n integer)".as_ptr(),
            result
        ),
        UQA_OK
    );
    assert_eq!(
        uqa_engine_execute(engine, c"SELECT 42".as_ptr(), result),
        UQA_OK
    );
    assert_eq!(
        uqa_statement_prepare(
            engine,
            c"INSERT INTO panic_effects VALUES (1) RETURNING ffi_native_panic(n)".as_ptr(),
            &raw mut statement
        ),
        UQA_OK
    );
    assert_eq!(
        uqa_cursor_open(
            engine,
            c"SELECT generate_series(1, 3)".as_ptr(),
            &raw mut cursor
        ),
        UQA_OK
    );
    let mut subscription = open_subscription(engine);
    if transaction {
        assert_eq!(uqa_transaction_begin(engine), UQA_OK);
        assert_eq!(
            uqa_engine_execute(
                engine,
                c"INSERT INTO panic_effects VALUES (0)".as_ptr(),
                result
            ),
            UQA_OK
        );
    }
    let mut copied = u64::MAX;
    let status = match operation {
        Operation::Message => uqa_engine_execute(engine, c"INSERT INTO panic_effects VALUES (1) RETURNING ffi_native_panic(n)".as_ptr(), result),
        Operation::Prepared => uqa_statement_execute(statement, result),
        Operation::Cursor => uqa_cursor_open(engine, c"WITH written AS (INSERT INTO panic_effects VALUES (1) RETURNING n) SELECT ffi_native_panic(n) FROM written".as_ptr(), &raw mut failed_cursor),
        Operation::Copy => uqa_engine_copy_to(engine, c"COPY (SELECT ffi_native_panic(1)) TO STDOUT".as_ptr(), Some(consume), None, ptr::null_mut(), &raw mut copied),
    };
    assert_eq!(
        status, UQA_ERROR_PANIC,
        "{operation:?}, transaction={transaction}"
    );
    assert!(failed_cursor.is_null());
    if matches!(operation, Operation::Copy) {
        assert_eq!(copied, 0);
    }
    verify_panic_diagnostic();
    assert_eq!(
        uqa_engine_execute(engine, c"SELECT 1".as_ptr(), result),
        UQA_ERROR_STATE
    );
    assert_eq!(uqa_statement_execute(statement, result), UQA_ERROR_STATE);
    assert_eq!(uqa_cursor_fetch(cursor, 1, result), UQA_ERROR_STATE);
    assert_eq!(uqa_engine_close(&raw mut engine), UQA_ERROR_BUSY);
    assert!(!engine.is_null());
    assert_eq!(
        uqa_subscription_poll(subscription, notification),
        UQA_TIMEOUT
    );
    assert_eq!(uqa_subscription_close(&raw mut subscription), UQA_OK);
    assert_eq!(uqa_notification_destroy(&raw mut notification), UQA_OK);
    assert_eq!(uqa_statement_finalize(&raw mut statement), UQA_OK);
    assert_eq!(uqa_cursor_close(&raw mut cursor), UQA_OK);
    assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
    assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
    assert!(engine.is_null());
    assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    verify_reopen(path, flags);
}

fn all_operations(flags: UQAOpenFlags) {
    for operation in [
        Operation::Message,
        Operation::Prepared,
        Operation::Cursor,
        Operation::Copy,
    ] {
        for transaction in [false, true] {
            unsafe {
                verify(flags, operation, transaction);
            }
        }
    }
}

#[test]
fn native_panics_preserve_cleanup_on_memory() {
    all_operations(UQA_OPEN_MEMORY);
}
#[test]
fn native_panics_preserve_cleanup_on_sqlite_native() {
    all_operations(UQA_OPEN_SQLITE_NATIVE);
}
#[test]
fn native_panics_preserve_cleanup_on_sqlite_kv() {
    all_operations(UQA_OPEN_SQLITE_KV);
}
#[test]
fn native_panics_preserve_cleanup_on_redb() {
    all_operations(UQA_OPEN_REDB);
}
