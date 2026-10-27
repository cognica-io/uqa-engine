//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Config(*mut UQAConfig);

impl Drop for Config {
    fn drop(&mut self) {
        let status = unsafe { uqa_config_destroy(&raw mut self.0) };
        if !std::thread::panicking() {
            assert_eq!(status, UQA_OK);
        }
    }
}

fn configurations(mut run: impl FnMut(UQAOpenFlags, *mut UQAConfig)) {
    for flags in [UQA_OPEN_SQLITE_NATIVE, UQA_OPEN_SQLITE_KV] {
        for encrypted in [false, true] {
            for compression in [
                UQA_COMPRESSION_NONE,
                UQA_COMPRESSION_LZ4,
                UQA_COMPRESSION_ZSTD,
            ] {
                let mut config = Config(ptr::null_mut());
                unsafe {
                    assert_eq!(uqa_config_create(&raw mut config.0), UQA_OK);
                    if encrypted {
                        assert_eq!(
                            uqa_config_set_encryption_key(config.0, c"completion-key".as_ptr()),
                            UQA_OK
                        );
                    }
                    assert_eq!(
                        uqa_config_set_compression(
                            config.0,
                            compression,
                            if compression == UQA_COMPRESSION_ZSTD {
                                3
                            } else {
                                0
                            }
                        ),
                        UQA_OK
                    );
                }
                run(flags, config.0);
            }
        }
    }
}

#[test]
fn every_sqlite_layout_and_container_recovers_commit_receipts_without_duplicate_rows() {
    configurations(|flags, config| {
        for boundary in [RecordBoundary::Before, RecordBoundary::After] {
            let mut database = Database::with_config(flags, config);
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
                    assert_eq!(uqa_transaction_commit(database.engine), UQA_ERROR_SQL);
                });
                assert_eq!(uqa_transaction_commit(database.engine), UQA_OK);
            }
            assert_eq!(database.state(), UQA_TRANSACTION_IDLE);
            database.reopen(config);
            assert_eq!(
                database.integer(c"SELECT count(*) FROM completion_items"),
                1
            );
            assert_eq!(
                database.integer(c"SELECT n::bigint FROM completion_items"),
                1
            );
        }
    });
}

#[test]
fn every_sqlite_layout_and_container_recovers_abort_receipts_without_publishing_rows() {
    configurations(|flags, config| {
        for boundary in [RecordBoundary::Before, RecordBoundary::After] {
            let mut database = Database::with_config(flags, config);
            database.sql(c"CREATE TABLE completion_items(n integer PRIMARY KEY)");
            database.sql(c"BEGIN; INSERT INTO completion_items VALUES (1)");
            unsafe {
                fail(RecordOperation::Commit, RecordBoundary::Before, || {
                    assert_eq!(uqa_transaction_commit(database.engine), UQA_ERROR_SQL);
                    assert_eq!(diagnostic().1, UQA_COMPLETION_PENDING);
                });
                fail(RecordOperation::Abort, boundary, || {
                    assert_eq!(uqa_transaction_rollback(database.engine), UQA_ERROR_SQL);
                    assert_eq!(diagnostic().1, UQA_COMPLETION_PENDING);
                    assert_eq!(uqa_transaction_rollback(database.engine), UQA_ERROR_SQL);
                });
                assert_eq!(uqa_transaction_rollback(database.engine), UQA_OK);
            }
            assert_eq!(database.state(), UQA_TRANSACTION_IDLE);
            database.reopen(config);
            assert_eq!(
                database.integer(c"SELECT count(*) FROM completion_items"),
                0
            );
        }
    });
}
