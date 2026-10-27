//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Failed temporary catalog publication keeps the C session and original completion receipt.

use super::*;

fn temporary_dependents(database: &Database) {
    database.sql(c"CREATE DOMAIN pg_temp.cleanup_domain AS integer; CREATE TABLE cleanup_items(n integer, d pg_temp.cleanup_domain); INSERT INTO cleanup_items VALUES(7,1); CREATE VIEW cleanup_view AS SELECT d FROM cleanup_items");
}

fn verify_cleanup(database: &mut Database) {
    database.reopen(ptr::null_mut());
    assert_eq!(database.integer(c"SELECT n FROM cleanup_items"), 7);
    assert_eq!(database.integer(c"SELECT count(*) FROM pg_attribute WHERE attrelid='cleanup_items'::regclass AND attnum>0 AND NOT attisdropped"), 1);
    assert_eq!(
        database.integer(c"SELECT count(*) FROM pg_type WHERE typname='cleanup_domain'"),
        0
    );
    assert_eq!(
        database.integer(c"SELECT count(*) FROM pg_class WHERE relname='cleanup_view'"),
        0
    );
}

#[derive(Clone, Copy, Debug)]
enum CloseEntry {
    Ordinary,
    Cancelled,
    Terminated,
    AutomaticTermination,
}

#[test]
fn temporary_cleanup_commit_failure_retains_the_handle_and_resolves_the_actual_outcome() {
    for flags in PERSISTENT {
        for boundary in [RecordBoundary::Before, RecordBoundary::After] {
            for entry in [
                CloseEntry::Ordinary,
                CloseEntry::Cancelled,
                CloseEntry::Terminated,
                CloseEntry::AutomaticTermination,
            ] {
                let mut database = Database::new(flags);
                temporary_dependents(&database);
                unsafe {
                    assert_eq!(uqa_result_destroy(&raw mut database.result), UQA_OK);
                    let original = database.engine;
                    let token = crate::test_support::with_native_engine(
                        original,
                        uqa_engine::Engine::cancellation_token,
                    )
                    .unwrap();
                    fail(RecordOperation::Commit, boundary, || {
                        match entry {
                            CloseEntry::Ordinary => {}
                            CloseEntry::Cancelled => token.cancel(),
                            CloseEntry::Terminated | CloseEntry::AutomaticTermination => {
                                token.cancel_with(uqa_core::CancellationReason::IdleSessionTimeout);
                            }
                        }
                        if matches!(entry, CloseEntry::AutomaticTermination) {
                            let error =
                                crate::test_support::with_native_engine(original, |native| {
                                    native.sql("SELECT 1", &[]).err().unwrap()
                                })
                                .unwrap();
                            assert_eq!(error.sqlstate(), Some("57P05"));
                            assert_eq!(database.state(), UQA_TRANSACTION_COMPLETION_PENDING);
                        }
                        for _ in 0..2 {
                            assert_eq!(uqa_engine_close(&raw mut database.engine), UQA_ERROR_SQL);
                            assert_eq!(database.engine, original);
                            let (state, completion, message) = diagnostic();
                            assert_eq!(
                                (state.as_str(), completion),
                                ("08007", UQA_COMPLETION_PENDING),
                                "{flags}, {boundary:?}, {entry:?}: {message}"
                            );
                            assert_eq!(database.state(), UQA_TRANSACTION_COMPLETION_PENDING);
                        }
                    });
                    if boundary == RecordBoundary::After {
                        assert_eq!(uqa_engine_close(&raw mut database.engine), UQA_ERROR_SQL);
                        assert_eq!(database.engine, original);
                        let (state, completion, message) = diagnostic();
                        assert_eq!(
                            (state.as_str(), completion),
                            ("25000", UQA_COMPLETION_COMMITTED),
                            "{flags}, {boundary:?}, {entry:?}: {message}"
                        );
                    }
                    assert_eq!(uqa_engine_close(&raw mut database.engine), UQA_OK);
                    assert!(database.engine.is_null());
                    if !matches!(entry, CloseEntry::Ordinary) {
                        assert!(token.check().is_err());
                    }
                }
                verify_cleanup(&mut database);
            }
        }
    }
}

#[test]
fn rejected_temporary_cleanup_retains_the_commit_cause_and_pending_abort() {
    for flags in PERSISTENT {
        for terminated in [false, true] {
            let mut database = Database::new(flags);
            temporary_dependents(&database);
            unsafe {
                assert_eq!(uqa_result_destroy(&raw mut database.result), UQA_OK);
                let original = database.engine;
                if terminated {
                    let token = crate::test_support::with_native_engine(
                        original,
                        uqa_engine::Engine::cancellation_token,
                    )
                    .unwrap();
                    token.cancel_with(uqa_core::CancellationReason::IdleSessionTimeout);
                }
                reject_commit_then_fail_abort(|| {
                    assert_eq!(uqa_engine_close(&raw mut database.engine), UQA_ERROR_SQL);
                    assert_eq!(database.engine, original);
                    let (state, completion, message) = diagnostic();
                    assert_eq!(
                        (state.as_str(), completion),
                        ("08007", UQA_COMPLETION_PENDING),
                        "{message}"
                    );
                    assert!(
                        message.contains(&uqa_core::memory::MemoryError::SizeOverflow.to_string()),
                        "original commit failure was lost: {message}"
                    );
                    assert_eq!(database.state(), UQA_TRANSACTION_COMPLETION_PENDING);
                });
                assert_eq!(uqa_engine_close(&raw mut database.engine), UQA_OK);
                assert!(database.engine.is_null());
            }
            verify_cleanup(&mut database);
        }
    }
}
