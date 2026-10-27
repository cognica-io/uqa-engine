//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

#[derive(Clone, Copy, Debug)]
enum Operation {
    Prepared,
    Cursor,
    CopyFrom,
    CopyTo,
}

#[derive(Default)]
struct Transfer {
    reads: usize,
    writes: usize,
    flushes: usize,
    position: usize,
    output: Vec<u8>,
}

unsafe extern "C" fn read(
    data: *mut c_void,
    bytes: *mut u8,
    capacity: usize,
    count: *mut usize,
) -> UQAStatus {
    let input = &mut *data.cast::<Transfer>();
    input.reads += 1;
    let source = &b"1\n"[input.position..];
    let copied = source.len().min(capacity);
    std::ptr::copy_nonoverlapping(source.as_ptr(), bytes, copied);
    input.position += copied;
    count.write(copied);
    UQA_OK
}

unsafe extern "C" fn write(
    data: *mut c_void,
    bytes: *const u8,
    size: usize,
    count: *mut usize,
) -> UQAStatus {
    let output = &mut *data.cast::<Transfer>();
    output.writes += 1;
    output
        .output
        .extend_from_slice(std::slice::from_raw_parts(bytes, size));
    count.write(size);
    UQA_OK
}

unsafe extern "C" fn flush(data: *mut c_void) -> UQAStatus {
    (*data.cast::<Transfer>()).flushes += 1;
    UQA_OK
}

unsafe fn prepare_probe(database: &Database) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    test_support::with_native_engine(database.engine, |engine| {
        engine
            .register_scalar_function("completion_probe", move |_: &[uqa_core::Value]| {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok(uqa_core::Value::Int(1))
            })
            .unwrap();
    })
    .unwrap();
    database.sql(c"CREATE TABLE completion_items (n integer PRIMARY KEY); CREATE FUNCTION completion_trigger() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM completion_probe(); PERFORM pg_notify('completion', NEW.n::text); RETURN NEW; END $$; CREATE TRIGGER completion_effect AFTER INSERT ON completion_items FOR EACH ROW EXECUTE FUNCTION completion_trigger(); CREATE FUNCTION completion_write() RETURNS integer LANGUAGE plpgsql AS $$ BEGIN INSERT INTO completion_items VALUES (1); RETURN 1; END $$");
    calls
}

unsafe fn subscribe(database: &Database) -> (*mut UQASubscription, *mut UQANotification) {
    let mut subscription = ptr::null_mut();
    let mut notification = ptr::null_mut();
    let channels = [c"completion".as_ptr()];
    let options = UQASubscriptionOptions {
        struct_size: size_of::<UQASubscriptionOptions>() as u32,
        version: 1,
        max_active_subscriptions: 8,
        max_channels: 8,
        max_queued_notifications: 16,
        max_queued_bytes: 4096,
        max_registry_entries_per_poll: 16,
    };
    assert_eq!(
        uqa_subscription_open(
            database.engine,
            channels.as_ptr(),
            1,
            &raw const options,
            &raw mut subscription
        ),
        UQA_OK
    );
    assert_eq!(uqa_notification_create(&raw mut notification), UQA_OK);
    (subscription, notification)
}

unsafe fn exercise(
    flags: UQAOpenFlags,
    operation: Operation,
    boundary: RecordBoundary,
    rejected: bool,
) {
    let mut database = Database::new(flags);
    let calls = prepare_probe(&database);
    let (mut subscription, mut notification) = subscribe(&database);
    let mut statement = ptr::null_mut();
    let mut cursor = ptr::null_mut();
    if matches!(operation, Operation::Prepared) {
        assert_eq!(
            uqa_statement_prepare(
                database.engine,
                c"INSERT INTO completion_items VALUES (1) RETURNING n".as_ptr(),
                &raw mut statement
            ),
            UQA_OK
        );
    }
    assert_eq!(database.integer(c"SELECT 31::bigint"), 31);
    let retained = database.bytes();
    let mut transfer = Transfer::default();
    let mut rows = u64::MAX;
    let user = (&raw mut transfer).cast();
    let attempt = || {
        let status = match operation {
            Operation::Prepared => uqa_statement_execute(statement, database.result),
            Operation::Cursor => uqa_cursor_open(database.engine, c"WITH inserted AS (INSERT INTO completion_items VALUES (1) RETURNING n) SELECT * FROM inserted".as_ptr(), &raw mut cursor),
            Operation::CopyFrom => uqa_engine_copy_from(database.engine, c"COPY completion_items FROM STDIN".as_ptr(), Some(read), user, &raw mut rows),
            Operation::CopyTo => uqa_engine_copy_to(database.engine, c"COPY (SELECT completion_write()) TO STDOUT".as_ptr(), Some(write), Some(flush), user, &raw mut rows),
        };
        assert_eq!(
            status,
            UQA_ERROR_SQL,
            "{flags}, {operation:?}, {boundary:?}: {:?}",
            diagnostic()
        );
        let (state, completion, message) = diagnostic();
        assert_eq!(
            (state.as_str(), completion),
            ("08007", UQA_COMPLETION_PENDING),
            "{flags}, {operation:?}, {boundary:?}: {message}"
        );
        if rejected {
            assert!(
                message.contains(&uqa_core::memory::MemoryError::SizeOverflow.to_string()),
                "original commit failure was lost: {message}"
            );
        }
        assert!(cursor.is_null());
        if matches!(operation, Operation::CopyFrom | Operation::CopyTo) {
            assert_eq!(rows, 0);
        }
        assert_eq!(database.retained_integer(), 31);
        assert_eq!(database.bytes(), retained);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(uqa_transaction_commit(database.engine), UQA_ERROR_SQL);
        assert_eq!(diagnostic().1, UQA_COMPLETION_PENDING);
    };
    if rejected {
        reject_commit_then_fail_abort(attempt);
    } else {
        fail(RecordOperation::Commit, boundary, attempt);
    }
    let callbacks = (transfer.reads, transfer.writes, transfer.flushes);
    assert_eq!(uqa_transaction_commit(database.engine), UQA_OK);
    assert_eq!(
        (transfer.reads, transfer.writes, transfer.flushes),
        callbacks
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    if matches!(operation, Operation::CopyFrom) {
        assert!(transfer.reads > 0);
    }
    if matches!(operation, Operation::CopyTo) {
        assert_eq!(transfer.output, b"1\n");
        assert!(transfer.writes > 0 && transfer.flushes > 0);
    }
    if !rejected {
        assert_eq!(uqa_subscription_poll(subscription, notification), UQA_OK);
        let mut sequence = 0;
        assert_eq!(
            uqa_notification_get_sequence(notification, &raw mut sequence),
            UQA_OK
        );
        assert!(sequence > 0);
    }
    assert_eq!(
        uqa_subscription_poll(subscription, notification),
        UQA_TIMEOUT
    );
    assert_eq!(uqa_statement_finalize(&raw mut statement), UQA_OK);
    assert_eq!(uqa_cursor_close(&raw mut cursor), UQA_OK);
    assert_eq!(uqa_subscription_close(&raw mut subscription), UQA_OK);
    assert_eq!(uqa_notification_destroy(&raw mut notification), UQA_OK);
    assert_eq!(
        database.integer(c"SELECT count(*) FROM completion_items"),
        i64::from(!rejected)
    );
    database.reopen(ptr::null_mut());
    assert_eq!(
        database.integer(c"SELECT count(*) FROM completion_items"),
        i64::from(!rejected)
    );
}

#[test]
fn prepared_cursor_and_copy_completion_resolve_once_with_notifications_and_reopen() {
    for flags in PERSISTENT {
        for operation in [
            Operation::Prepared,
            Operation::Cursor,
            Operation::CopyFrom,
            Operation::CopyTo,
        ] {
            for boundary in [RecordBoundary::Before, RecordBoundary::After] {
                unsafe {
                    exercise(flags, operation, boundary, false);
                }
            }
        }
    }
}

#[test]
fn rejected_commit_with_failed_abort_retains_the_primary_error_and_rolls_back_on_retry() {
    for flags in PERSISTENT {
        for operation in [
            Operation::Prepared,
            Operation::Cursor,
            Operation::CopyFrom,
            Operation::CopyTo,
        ] {
            unsafe { exercise(flags, operation, RecordBoundary::Before, true) };
        }
    }
}
