//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! C callback factories and result publication retain exactly-once ownership under allocation failure.

use super::{
    fixtures::{Database, PROVIDERS},
    target,
};
use std::{
    ffi::{c_void, CStr},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};
use uqa_ffi::*;

#[derive(Default)]
struct Host {
    value: *mut UQAValue,
    retired: AtomicUsize,
    created: AtomicUsize,
    closed: AtomicUsize,
    calls: AtomicUsize,
}

impl Host {
    fn new() -> Self {
        let mut host = Self::default();
        unsafe {
            assert_eq!(uqa_value_create_int32(17, &raw mut host.value), UQA_OK);
        }
        host
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        unsafe {
            let _ = uqa_value_destroy(&raw mut self.value);
        }
    }
}

unsafe extern "C" fn retire(data: *mut c_void) -> UQAStatus {
    (*data.cast::<Host>())
        .retired
        .fetch_add(1, Ordering::SeqCst);
    UQA_OK
}

unsafe extern "C" fn scalar(
    context: *mut UQAUDFContext,
    _: *const *const UQAValue,
    _: u32,
    data: *mut c_void,
) -> UQAStatus {
    let host = &*data.cast::<Host>();
    host.calls.fetch_add(1, Ordering::SeqCst);
    uqa_udf_context_set_result(context, host.value)
}

unsafe extern "C" fn create(data: *mut c_void, state: *mut *mut c_void) -> UQAStatus {
    (*data.cast::<Host>())
        .created
        .fetch_add(1, Ordering::SeqCst);
    state.write(data);
    UQA_OK
}

unsafe extern "C" fn create_scan(
    _: *const *const UQAValue,
    _: u32,
    data: *mut c_void,
    state: *mut *mut c_void,
) -> UQAStatus {
    create(data, state)
}

unsafe extern "C" fn close(state: *mut c_void, data: *mut c_void) -> UQAStatus {
    if state != data {
        return UQA_ERROR_STATE;
    }
    (*data.cast::<Host>()).closed.fetch_add(1, Ordering::SeqCst);
    UQA_OK
}

unsafe extern "C" fn next(
    sink: *mut UQAUDFRowSink,
    _: *mut c_void,
    data: *mut c_void,
) -> UQAStatus {
    let host = &*data.cast::<Host>();
    if host.calls.fetch_add(1, Ordering::SeqCst) != 0 {
        return UQA_DONE;
    }
    uqa_udf_row_sink_emit(sink, [host.value.cast_const()].as_ptr(), 1)
}

unsafe extern "C" fn observe(
    _: *mut c_void,
    _: *const *const UQAValue,
    _: u32,
    data: *mut c_void,
) -> UQAStatus {
    (*data.cast::<Host>()).calls.fetch_add(1, Ordering::SeqCst);
    UQA_OK
}

unsafe extern "C" fn finish(
    context: *mut UQAUDFContext,
    _: *mut c_void,
    data: *mut c_void,
) -> UQAStatus {
    uqa_udf_context_set_result(context, (*data.cast::<Host>()).value)
}

#[derive(Clone, Copy)]
enum Kind {
    Scalar,
    Table,
    Aggregate,
}

impl Kind {
    unsafe fn register(self, db: &Database, host: &mut Host, out: *mut *mut UQAUDF) -> UQAStatus {
        let data = ptr::from_mut(host).cast();
        match self {
            Self::Scalar => uqa_udf_register_scalar(
                db.engine,
                c"allocation_function".as_ptr(),
                ptr::null_mut(),
                Some(scalar),
                data,
                Some(retire),
                out,
            ),
            Self::Table => uqa_udf_register_table(
                db.engine,
                c"allocation_function".as_ptr(),
                ptr::null_mut(),
                [c"n".as_ptr()].as_ptr(),
                1,
                Some(create_scan),
                Some(next),
                Some(close),
                data,
                Some(retire),
                out,
            ),
            Self::Aggregate => uqa_udf_register_aggregate(
                db.engine,
                c"allocation_function".as_ptr(),
                ptr::null_mut(),
                Some(create),
                Some(observe),
                Some(finish),
                Some(close),
                data,
                Some(retire),
                out,
            ),
        }
    }

    fn sql(self) -> &'static CStr {
        match self {
            Self::Scalar | Self::Aggregate => c"SELECT allocation_function(7)",
            Self::Table => c"SELECT * FROM allocation_function()",
        }
    }
}

#[test]
fn failed_registration_leaves_user_data_with_the_caller_for_every_function_kind() {
    for flags in PROVIDERS {
        for kind in [Kind::Scalar, Kind::Table, Kind::Aggregate] {
            let mut host = Host::new();
            let db = Database::new(flags);
            let bytes = db.bytes();
            let mut function = ptr::null_mut();
            unsafe {
                assert_eq!(
                    target::reject::<UQAUDF, _>(|| kind.register(
                        &db,
                        &mut host,
                        &raw mut function
                    )),
                    UQA_ERROR_OUT_OF_MEMORY
                );
                assert!(function.is_null());
                assert_eq!(host.retired.load(Ordering::SeqCst), 0);
                assert_eq!(db.bytes(), bytes);
                assert_eq!(kind.register(&db, &mut host, &raw mut function), UQA_OK);
                assert_eq!(uqa_udf_unregister(&raw mut function), UQA_OK);
                assert!(function.is_null());
                assert_eq!(host.retired.load(Ordering::SeqCst), 1);
                assert_eq!(db.bytes(), bytes);
            }
        }
    }
}

unsafe fn failed_invocation(
    flags: UQAOpenFlags,
    kind: Kind,
    target: &'static str,
    skip: usize,
    created: usize,
) {
    let mut host = Host::new();
    let db = Database::new(flags);
    let mut function = ptr::null_mut();
    assert_eq!(kind.register(&db, &mut host, &raw mut function), UQA_OK);
    db.sql(c"BEGIN; SAVEPOINT callback_failure; SELECT 42");
    let bytes = db.bytes();
    let status = target::reject_named(target, skip, || {
        uqa_engine_execute(db.engine, kind.sql().as_ptr(), db.result)
    });
    verify_callback_allocation_failure(status);
    assert_eq!(db.state(), UQA_TRANSACTION_FAILED);
    assert_eq!(host.created.load(Ordering::SeqCst), created);
    assert_eq!(host.closed.load(Ordering::SeqCst), created);
    assert_eq!(host.retired.load(Ordering::SeqCst), 0);
    assert_eq!(db.bytes(), bytes);
    let mut value = ptr::null();
    let mut number = 0;
    assert_eq!(
        uqa_result_get_value(db.result, 0, 0, &raw mut value),
        UQA_OK
    );
    assert_eq!(uqa_value_get_int64(value, &raw mut number), UQA_OK);
    assert_eq!(number, 42);
    assert_eq!(
        uqa_savepoint_rollback(db.engine, c"callback_failure".as_ptr()),
        UQA_OK
    );
    assert_eq!(db.state(), UQA_TRANSACTION_ACTIVE);
    host.calls.store(0, Ordering::SeqCst);
    assert_eq!(db.integer(kind.sql()), 17);
    assert_eq!(
        host.created.load(Ordering::SeqCst),
        host.closed.load(Ordering::SeqCst)
    );
    assert_eq!(uqa_transaction_rollback(db.engine), UQA_OK);
    assert_eq!(uqa_udf_unregister(&raw mut function), UQA_OK);
    assert_eq!(host.retired.load(Ordering::SeqCst), 1);
}

#[test]
fn callback_argument_state_and_result_failures_close_created_resources_and_recover_at_savepoints() {
    let value = std::any::type_name::<UQAValue>();
    for flags in PROVIDERS {
        for (kind, target, skip, created) in [
            (Kind::Scalar, value, 0, 0),
            (Kind::Scalar, value, 1, 0),
            (Kind::Table, "uqa_ffi::udf::table::Scan", 0, 0),
            (Kind::Table, value, 0, 1),
            (Kind::Aggregate, "uqa_ffi::udf::aggregate::State", 0, 0),
            (Kind::Aggregate, value, 0, 1),
            (Kind::Aggregate, value, 1, 1),
        ] {
            unsafe {
                failed_invocation(flags, kind, target, skip, created);
            }
        }
    }
}

unsafe fn verify_callback_allocation_failure(status: UQAStatus) {
    assert_eq!(status, UQA_ERROR_SQL);
    let mut error = ptr::null();
    let mut cause = ptr::null();
    let mut cause_status = UQA_OK;
    assert_eq!(uqa_error_get_last(&raw mut error), UQA_OK);
    assert_eq!(uqa_error_get_cause(error, &raw mut cause), UQA_OK);
    assert_eq!(uqa_error_get_status(cause, &raw mut cause_status), UQA_OK);
    assert_eq!(cause_status, UQA_ERROR_OUT_OF_MEMORY);
}

unsafe fn failed_domain_input(flags: UQAOpenFlags, skip: usize, transaction: bool) {
    let mut host = Host::default();
    assert_eq!(uqa_value_create_bool(1, &raw mut host.value), UQA_OK);
    let db = Database::new(flags);
    let mut function = ptr::null_mut();
    assert_eq!(
        Kind::Scalar.register(&db, &mut host, &raw mut function),
        UQA_OK
    );
    db.sql(c"CREATE TABLE domain_effects(n integer); CREATE FUNCTION audited_input(p integer) RETURNS boolean LANGUAGE plpgsql AS $$ BEGIN INSERT INTO domain_effects VALUES (p); RETURN allocation_function(p); END $$; CREATE DOMAIN callback_input AS integer CHECK(audited_input(VALUE))");
    if transaction {
        db.sql(c"BEGIN; SAVEPOINT domain_input");
    }
    let mut value = ptr::null_mut();
    let bytes = db.bytes();
    let status = target::reject_named(std::any::type_name::<UQAValue>(), skip, || {
        uqa_value_create_from_text(
            db.engine,
            c"callback_input".as_ptr(),
            c"17".as_ptr(),
            &raw mut value,
        )
    });
    verify_callback_allocation_failure(status);
    assert!(value.is_null());
    assert_eq!(
        db.state(),
        if transaction {
            UQA_TRANSACTION_FAILED
        } else {
            UQA_TRANSACTION_IDLE
        }
    );
    assert_eq!(db.bytes(), bytes);
    if transaction {
        assert_eq!(
            uqa_savepoint_rollback(db.engine, c"domain_input".as_ptr()),
            UQA_OK
        );
    }
    assert_eq!(db.integer(c"SELECT count(*) FROM domain_effects"), 0);
    assert_eq!(
        uqa_value_create_from_text(
            db.engine,
            c"callback_input".as_ptr(),
            c"17".as_ptr(),
            &raw mut value
        ),
        UQA_OK
    );
    assert_eq!(db.integer(c"SELECT n FROM domain_effects"), 17);
    assert_eq!(uqa_value_destroy(&raw mut value), UQA_OK);
    if transaction {
        assert_eq!(uqa_transaction_rollback(db.engine), UQA_OK);
    }
    assert_eq!(uqa_udf_unregister(&raw mut function), UQA_OK);
    assert_eq!(host.retired.load(Ordering::SeqCst), 1);
}

#[test]
fn catalog_input_callback_allocation_failures_rollback_domain_effects_on_every_provider() {
    for flags in PROVIDERS {
        // The first handle belongs to contextual input; reject the later callback argument or result instead.
        for skip in [1, 2] {
            for transaction in [false, true] {
                unsafe {
                    failed_domain_input(flags, skip, transaction);
                }
            }
        }
    }
}
