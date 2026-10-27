//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Callback allocation and cleanup failures share the native request's rollback owner.

use super::*;
use std::ffi::CString;

#[path = "combined/notifications.rs"]
mod notifications;
use notifications::Events;

#[derive(Clone, Copy, Debug)]
enum Entry {
    Message,
    Prepared,
    Cursor,
    Copy,
}

struct Request {
    entry: Entry,
    sql: CString,
    statement: *mut UQAStatement,
    cursor: *mut UQACursor,
    bytes: usize,
}

unsafe extern "C" fn write(
    data: *mut c_void,
    _: *const u8,
    size: usize,
    written: *mut usize,
) -> UQAStatus {
    *data.cast::<usize>() += size;
    written.write(size);
    UQA_OK
}

impl Request {
    unsafe fn new(db: &Database, entry: Entry, kind: Kind) -> Self {
        let query = match kind {
            Kind::Scalar | Kind::Aggregate => "SELECT allocation_function(combined_effect())",
            Kind::Table => "SELECT * FROM allocation_function(combined_effect())",
        };
        let sql = CString::new(if matches!(entry, Entry::Copy) {
            format!("COPY ({query}) TO STDOUT")
        } else {
            query.into()
        })
        .unwrap();
        let mut request = Self {
            entry,
            sql,
            statement: ptr::null_mut(),
            cursor: ptr::null_mut(),
            bytes: 0,
        };
        if matches!(entry, Entry::Prepared) {
            assert_eq!(
                uqa_statement_prepare(db.engine, request.sql.as_ptr(), &raw mut request.statement),
                UQA_OK
            );
        }
        request
    }

    unsafe fn execute(&mut self, db: &Database) -> UQAStatus {
        match self.entry {
            Entry::Message => uqa_engine_execute(db.engine, self.sql.as_ptr(), db.result),
            Entry::Prepared => uqa_statement_execute(self.statement, db.result),
            Entry::Cursor => {
                let opened = uqa_cursor_open(db.engine, self.sql.as_ptr(), &raw mut self.cursor);
                if opened != UQA_OK {
                    return opened;
                }
                uqa_cursor_fetch(self.cursor, 1, db.result)
            }
            Entry::Copy => {
                let mut rows = u64::MAX;
                let status = uqa_engine_copy_to(
                    db.engine,
                    self.sql.as_ptr(),
                    Some(write),
                    None,
                    (&raw mut self.bytes).cast(),
                    &raw mut rows,
                );
                if status == UQA_OK {
                    assert_eq!(rows, 1);
                    assert_eq!(self.bytes, 3);
                } else {
                    assert_eq!(rows, 0);
                    assert_eq!(self.bytes, 0);
                }
                status
            }
        }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        unsafe {
            let statement = uqa_statement_finalize(&raw mut self.statement);
            let cursor = uqa_cursor_close(&raw mut self.cursor);
            if !std::thread::panicking() {
                assert_eq!((statement, cursor), (UQA_OK, UQA_OK));
            }
        }
    }
}

unsafe fn diagnostic(transaction: bool) {
    let mut error = ptr::null();
    let mut detail = UQAStringView::default();
    let mut completion = u32::MAX;
    assert_eq!(uqa_error_get_last(&raw mut error), UQA_OK);
    assert_eq!(uqa_error_get_detail(error, &raw mut detail), UQA_OK);
    let detail = std::str::from_utf8(std::slice::from_raw_parts(
        detail.data.cast::<u8>(),
        detail.size,
    ))
    .unwrap();
    assert!(detail.contains("combined cleanup failure"), "{detail}");
    assert_eq!(uqa_error_get_completion(error, &raw mut completion), UQA_OK);
    assert_eq!(
        completion,
        if transaction {
            UQA_COMPLETION_IN_TRANSACTION
        } else {
            UQA_COMPLETION_ROLLED_BACK
        }
    );
}

unsafe fn allocation_with_cleanup(
    flags: UQAOpenFlags,
    kind: Kind,
    entry: Entry,
    skip: usize,
    transaction: bool,
) {
    let mut host = Host::new();
    host.cleanup_failure.store(true, Ordering::SeqCst);
    let mut db = Database::new(flags);
    let events = Events::new(&db);
    let mut function = ptr::null_mut();
    assert_eq!(kind.register(&db, &mut host, &raw mut function), UQA_OK);
    db.sql(c"CREATE TABLE combined_effects(n integer); CREATE FUNCTION combined_effect() RETURNS integer LANGUAGE plpgsql AS $$ BEGIN INSERT INTO combined_effects VALUES(17); PERFORM pg_notify('combined_events','committed'); RETURN 17; END $$");
    if transaction {
        db.sql(c"BEGIN; SAVEPOINT before_failure");
    }
    assert_eq!(db.integer(c"SELECT 42::bigint"), 42);
    let bytes = db.bytes();
    {
        let mut request = Request::new(&db, entry, kind);
        let status = target::reject_named(std::any::type_name::<UQAValue>(), skip, || {
            request.execute(&db)
        });
        verify_callback_allocation_failure(status);
        diagnostic(transaction);
        assert_eq!(host.created.load(Ordering::SeqCst), 1, "{flags}, {entry:?}");
        assert_eq!(host.closed.load(Ordering::SeqCst), 1, "{flags}, {entry:?}");
        assert_eq!(db.retained_integer(), 42);
        assert_eq!(
            db.state(),
            if transaction {
                UQA_TRANSACTION_FAILED
            } else {
                UQA_TRANSACTION_IDLE
            }
        );
    }
    assert_eq!(db.bytes(), bytes);
    events.absent();
    if transaction {
        assert_eq!(
            uqa_savepoint_rollback(db.engine, c"before_failure".as_ptr()),
            UQA_OK
        );
    }
    assert_eq!(db.integer(c"SELECT count(*) FROM combined_effects"), 0);
    events.absent();
    host.cleanup_failure.store(false, Ordering::SeqCst);
    host.calls.store(0, Ordering::SeqCst);
    {
        let mut retry = Request::new(&db, entry, kind);
        assert_eq!(retry.execute(&db), UQA_OK, "{flags}, {entry:?}");
        if !matches!(entry, Entry::Copy) {
            assert_eq!(db.retained_integer(), 17);
        }
    }
    assert_eq!(host.created.load(Ordering::SeqCst), 2);
    assert_eq!(host.closed.load(Ordering::SeqCst), 2);
    assert_eq!(db.integer(c"SELECT count(*) FROM combined_effects"), 1);
    if transaction {
        events.absent();
        assert_eq!(uqa_transaction_rollback(db.engine), UQA_OK);
        events.absent();
    } else {
        events.receive_after_rejected_transfer(&db);
    }
    assert_eq!(uqa_udf_unregister(&raw mut function), UQA_OK);
    assert_eq!(host.retired.load(Ordering::SeqCst), 1);
    drop(events);
    if flags != UQA_OPEN_MEMORY {
        db.reopen(ptr::null_mut());
        assert_eq!(
            db.integer(c"SELECT count(*) FROM combined_effects"),
            i64::from(!transaction)
        );
    }
}

#[test]
fn allocation_and_cleanup_failures_keep_the_primary_error_and_native_rollback() {
    for flags in PROVIDERS {
        for entry in [Entry::Message, Entry::Prepared, Entry::Cursor, Entry::Copy] {
            for (kind, skip) in [(Kind::Table, 1), (Kind::Aggregate, 0), (Kind::Aggregate, 1)] {
                for transaction in [false, true] {
                    unsafe {
                        allocation_with_cleanup(flags, kind, entry, skip, transaction);
                    }
                }
            }
        }
    }
}

unsafe fn panic_with_cleanup(flags: UQAOpenFlags, entry: Entry, transaction: bool) {
    let mut host = Host::new();
    host.cleanup_failure.store(true, Ordering::SeqCst);
    let mut db = Database::new(flags);
    let events = Events::new(&db);
    let mut function = ptr::null_mut();
    assert_eq!(
        Kind::Table.register(&db, &mut host, &raw mut function),
        UQA_OK
    );
    test_support::with_native_engine(db.engine, |native| {
        native
            .register_scalar_function(
                "combined_panic",
                |_: &[uqa_core::Value]| -> Result<uqa_core::Value, uqa_sql::SQLError> {
                    panic!("native panic with live C scan");
                },
            )
            .unwrap();
    })
    .unwrap();
    db.sql(c"CREATE TABLE combined_effects(n integer); CREATE FUNCTION combined_effect() RETURNS integer LANGUAGE plpgsql AS $$ BEGIN INSERT INTO combined_effects VALUES(17); PERFORM pg_notify('combined_events','committed'); RETURN 17; END $$");
    if transaction {
        db.sql(c"BEGIN");
    }
    // Demand one row so the panic occurs while the C scan is still live, before normal EOF cleanup can fail on a larger input batch.
    let query = "SELECT combined_panic(n) FROM allocation_function(combined_effect()) LIMIT 1";
    let sql = CString::new(if matches!(entry, Entry::Copy) {
        format!("COPY ({query}) TO STDOUT")
    } else {
        query.into()
    })
    .unwrap();
    let mut request = Request {
        entry,
        sql,
        statement: ptr::null_mut(),
        cursor: ptr::null_mut(),
        bytes: 0,
    };
    if matches!(entry, Entry::Prepared) {
        assert_eq!(
            uqa_statement_prepare(db.engine, request.sql.as_ptr(), &raw mut request.statement),
            UQA_OK
        );
    }
    assert_eq!(
        request.execute(&db),
        UQA_ERROR_PANIC,
        "{flags}, {entry:?}, {transaction}"
    );
    let mut error = ptr::null();
    let mut completion = u32::MAX;
    let mut message = UQAStringView::default();
    assert_eq!(uqa_error_get_last(&raw mut error), UQA_OK);
    assert_eq!(uqa_error_get_completion(error, &raw mut completion), UQA_OK);
    assert_eq!(completion, UQA_COMPLETION_UNKNOWN);
    assert_eq!(uqa_error_get_message(error, &raw mut message), UQA_OK);
    assert_eq!(
        std::slice::from_raw_parts(message.data.cast::<u8>(), message.size),
        b"panic at the C ABI boundary"
    );
    assert_eq!(host.created.load(Ordering::SeqCst), 1);
    assert_eq!(host.closed.load(Ordering::SeqCst), 1);
    events.absent();
    assert_eq!(
        uqa_engine_execute(db.engine, c"SELECT 1".as_ptr(), db.result),
        UQA_ERROR_STATE
    );
    drop(request);
    assert_eq!(uqa_udf_unregister(&raw mut function), UQA_OK);
    assert_eq!(host.retired.load(Ordering::SeqCst), 1);
    events.absent();
    drop(events);
    if flags != UQA_OPEN_MEMORY {
        db.reopen(ptr::null_mut());
        assert_eq!(db.integer(c"SELECT count(*) FROM combined_effects"), 0);
    }
}

#[test]
fn native_panics_with_failing_scan_cleanup_remain_poisoned_and_rollback_on_close() {
    for flags in PROVIDERS {
        for entry in [Entry::Message, Entry::Prepared, Entry::Cursor, Entry::Copy] {
            for transaction in [false, true] {
                unsafe {
                    panic_with_cleanup(flags, entry, transaction);
                }
            }
        }
    }
}
