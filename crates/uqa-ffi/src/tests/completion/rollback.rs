//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn rollback_resolves_the_actual_receipt_instead_of_claiming_an_unconfirmed_abort() {
    for flags in PERSISTENT {
        for boundary in [RecordBoundary::Before, RecordBoundary::After] {
            let mut database = Database::new(flags);
            database.sql(c"CREATE TABLE completion_items(n integer PRIMARY KEY)");
            unsafe {
                fail(RecordOperation::Commit, boundary, || {
                    assert_eq!(
                        uqa_engine_execute(
                            database.engine,
                            c"INSERT INTO completion_items VALUES (1)".as_ptr(),
                            database.result
                        ),
                        UQA_ERROR_SQL
                    );
                    assert_eq!(diagnostic().1, UQA_COMPLETION_PENDING);
                    assert_eq!(uqa_transaction_rollback(database.engine), UQA_ERROR_SQL);
                    assert_eq!(diagnostic().1, UQA_COMPLETION_PENDING);
                });
                let status = uqa_transaction_rollback(database.engine);
                if boundary == RecordBoundary::After {
                    assert_eq!(status, UQA_ERROR_SQL);
                    let (state, completion, message) = diagnostic();
                    assert_eq!(
                        (state.as_str(), completion),
                        ("25000", UQA_COMPLETION_COMMITTED),
                        "{message}"
                    );
                } else {
                    assert_eq!(status, UQA_OK);
                }
            }
            assert_eq!(database.state(), UQA_TRANSACTION_IDLE);
            let expected = i64::from(boundary == RecordBoundary::After);
            assert_eq!(
                database.integer(c"SELECT count(*) FROM completion_items"),
                expected
            );
            database.reopen(ptr::null_mut());
            assert_eq!(
                database.integer(c"SELECT count(*) FROM completion_items"),
                expected
            );
        }
    }
}

#[test]
fn failed_abort_preserves_the_engine_handle_and_retry_releases_the_physical_owner() {
    for flags in PERSISTENT {
        for boundary in [RecordBoundary::Before, RecordBoundary::After] {
            let mut database = Database::new(flags);
            database.sql(c"CREATE TABLE completion_items(n integer PRIMARY KEY)");
            database.sql(c"BEGIN; INSERT INTO completion_items VALUES (1)");
            unsafe {
                // Private rows need no physical abort until publication allocates a durable identity.
                fail(RecordOperation::Commit, RecordBoundary::Before, || {
                    assert_eq!(uqa_transaction_commit(database.engine), UQA_ERROR_SQL);
                    assert_eq!(diagnostic().1, UQA_COMPLETION_PENDING);
                });
                assert_eq!(uqa_result_destroy(&raw mut database.result), UQA_OK);
                let original = database.engine;
                fail(RecordOperation::Abort, boundary, || {
                    assert_eq!(uqa_engine_close(&raw mut database.engine), UQA_ERROR_SQL);
                    assert_eq!(database.engine, original);
                    let (state, completion, message) = diagnostic();
                    assert_eq!(
                        (state.as_str(), completion),
                        ("08007", UQA_COMPLETION_PENDING),
                        "{flags}, {boundary:?}: {message}"
                    );
                    assert_eq!(database.state(), UQA_TRANSACTION_COMPLETION_PENDING);
                    assert_eq!(uqa_engine_close(&raw mut database.engine), UQA_ERROR_SQL);
                    assert_eq!(database.engine, original);
                });
                assert_eq!(uqa_engine_close(&raw mut database.engine), UQA_OK);
                assert!(database.engine.is_null());
            }
            database.reopen(ptr::null_mut());
            assert_eq!(
                database.integer(c"SELECT count(*) FROM completion_items"),
                0
            );
        }
    }
}
