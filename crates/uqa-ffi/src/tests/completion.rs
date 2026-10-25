//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Real provider completion failures behind the ordinary C entry points.

#![allow(unsafe_code)]

use super::database::Database;
use crate::*;
use std::{cell::Cell, ptr};
use uqa_engine::storage_test_support::{
    with_record_faults, RecordBoundary, RecordEvent, RecordFault, RecordOperation,
};

const PERSISTENT: [UQAOpenFlags; 3] = [UQA_OPEN_SQLITE_NATIVE, UQA_OPEN_SQLITE_KV, UQA_OPEN_REDB];

#[path = "completion/operations.rs"]
mod operations;

#[path = "completion/rollback.rs"]
mod rollback;

#[path = "completion/configurations.rs"]
mod configurations;

#[derive(Clone, Copy)]
struct Failure {
    operation: RecordOperation,
    boundary: RecordBoundary,
    target: Option<RecordEvent>,
    triggered: bool,
    reject_commit: bool,
}

thread_local! {
    static FAILURE: Cell<Option<Failure>> = const { Cell::new(None) };
}

fn observe(event: RecordEvent) -> Option<RecordFault> {
    FAILURE.with(|slot| {
        let mut failure = slot.get().expect("scoped native failure");
        if failure.target.is_none() && event.operation == failure.operation {
            failure.target = Some(event);
        }
        if failure
            .target
            .is_none_or(|target| target.transaction != event.transaction)
        {
            slot.set(Some(failure));
            return None;
        }
        let fault = if failure.triggered && event.boundary == RecordBoundary::Before {
            Some(RecordFault::unknown(
                "injected unavailable native completion",
            ))
        } else if !failure.triggered
            && event.operation == failure.operation
            && event.boundary == failure.boundary
        {
            failure.triggered = true;
            Some(if failure.reject_commit {
                RecordFault::Reject(uqa_core::memory::MemoryError::SizeOverflow.into())
            } else {
                RecordFault::unknown("injected missing native completion reply")
            })
        } else {
            None
        };
        slot.set(Some(failure));
        fault
    })
}

fn fail<T>(operation: RecordOperation, boundary: RecordBoundary, body: impl FnOnce() -> T) -> T {
    fail_with(operation, boundary, false, body)
}

fn reject_commit_then_fail_abort<T>(body: impl FnOnce() -> T) -> T {
    fail_with(RecordOperation::Commit, RecordBoundary::Before, true, body)
}

fn fail_with<T>(
    operation: RecordOperation,
    boundary: RecordBoundary,
    reject_commit: bool,
    body: impl FnOnce() -> T,
) -> T {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            FAILURE.with(|slot| slot.set(None));
        }
    }
    assert!(FAILURE
        .with(|slot| slot.replace(Some(Failure {
            operation,
            boundary,
            target: None,
            triggered: false,
            reject_commit,
        })))
        .is_none());
    let restore = Restore;
    let result = with_record_faults(observe, body);
    assert!(
        FAILURE.with(|slot| slot.get().unwrap().triggered),
        "native completion boundary was not reached"
    );
    drop(restore);
    result
}

unsafe fn diagnostic() -> (String, u32, String) {
    let mut error = ptr::null();
    let mut sqlstate = UQAStringView::default();
    let mut completion = u32::MAX;
    let mut message = UQAStringView::default();
    assert_eq!(uqa_error_get_last(&raw mut error), UQA_OK);
    assert_eq!(uqa_error_get_sqlstate(error, &raw mut sqlstate), UQA_OK);
    assert_eq!(uqa_error_get_completion(error, &raw mut completion), UQA_OK);
    assert_eq!(uqa_error_get_message(error, &raw mut message), UQA_OK);
    let text = |value: UQAStringView| {
        if value.size == 0 {
            String::new()
        } else {
            String::from_utf8(
                std::slice::from_raw_parts(value.data.cast::<u8>(), value.size).to_vec(),
            )
            .unwrap()
        }
    };
    (text(sqlstate), completion, text(message))
}

#[test]
fn native_commit_reply_loss_retains_the_result_and_resolves_without_replaying_sql() {
    for flags in PERSISTENT {
        for boundary in [RecordBoundary::Before, RecordBoundary::After] {
            let database = Database::new(flags);
            database.sql(c"CREATE TABLE completion_items (n integer PRIMARY KEY)");
            assert_eq!(database.integer(c"SELECT 31::bigint"), 31);
            let retained = database.bytes();
            unsafe {
                fail(RecordOperation::Commit, boundary, || {
                    assert_eq!(
                        uqa_engine_execute(
                            database.engine,
                            c"INSERT INTO completion_items VALUES (1) RETURNING n".as_ptr(),
                            database.result
                        ),
                        UQA_ERROR_SQL
                    );
                    let (state, completion, message) = diagnostic();
                    assert_eq!(
                        (state.as_str(), completion),
                        ("08007", UQA_COMPLETION_PENDING),
                        "{flags}, {boundary:?}: {message}"
                    );
                    assert_eq!(database.state(), UQA_TRANSACTION_COMPLETION_PENDING);
                    assert_eq!(database.retained_integer(), 31);
                    assert_eq!(database.bytes(), retained);
                    assert_eq!(
                        uqa_engine_execute(database.engine, c"SELECT 1".as_ptr(), database.result),
                        UQA_ERROR_SQL
                    );
                    assert_eq!(diagnostic().0, "08007");
                    assert_eq!(uqa_transaction_commit(database.engine), UQA_ERROR_SQL);
                    assert_eq!(diagnostic().1, UQA_COMPLETION_PENDING);
                });
                assert_eq!(uqa_transaction_commit(database.engine), UQA_OK);
            }
            assert_eq!(database.state(), UQA_TRANSACTION_IDLE);
            assert_eq!(
                database.integer(c"SELECT count(*) FROM completion_items"),
                1
            );
            assert_eq!(
                database.integer(c"SELECT sum(n)::bigint FROM completion_items"),
                1
            );
        }
    }
}
