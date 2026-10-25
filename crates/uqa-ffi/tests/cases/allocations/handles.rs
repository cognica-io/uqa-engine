//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native construction and contextual input release leases after real handle allocation failures.

use super::{
    fixtures::{Database, PROVIDERS},
    target,
};
use std::{
    ffi::{c_char, CString},
    ptr,
};
use uqa_ffi::*;

unsafe fn fail_then_open(
    path: *const c_char,
    flags: UQAOpenFlags,
    config: *mut UQAConfig,
) -> *mut UQAEngine {
    let mut engine = ptr::null_mut();
    let status =
        target::reject::<UQAEngine, _>(|| uqa_engine_open(path, flags, config, &raw mut engine));
    assert_eq!(status, UQA_ERROR_OUT_OF_MEMORY, "provider flags {flags}");
    assert!(engine.is_null());
    assert_eq!(
        uqa_engine_open(path, flags, config, &raw mut engine),
        UQA_OK,
        "native constructor leaked a provider lock"
    );
    engine
}

unsafe fn open_configuration(flags: UQAOpenFlags, encrypted: bool, compression: u32) {
    let directory = tempfile::tempdir().unwrap();
    let path = CString::new(directory.path().join("failed-open.db").to_str().unwrap()).unwrap();
    let path = if flags == UQA_OPEN_MEMORY {
        ptr::null()
    } else {
        path.as_ptr()
    };
    let mut config = ptr::null_mut();
    let mut result = ptr::null_mut();
    assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
    assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
    if encrypted {
        assert_eq!(
            uqa_config_set_encryption_key(config, c"allocation-open-key".as_ptr()),
            UQA_OK
        );
    }
    assert_eq!(
        uqa_config_set_compression(
            config,
            compression,
            if compression == UQA_COMPRESSION_ZSTD {
                3
            } else {
                0
            }
        ),
        UQA_OK
    );
    let mut engine = fail_then_open(path, flags, config);
    assert_eq!(
        uqa_engine_execute(
            engine,
            c"CREATE TABLE kept(n integer); INSERT INTO kept VALUES (17)".as_ptr(),
            result
        ),
        UQA_OK
    );
    assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
    assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
    if flags != UQA_OPEN_MEMORY {
        let flags = if flags == UQA_OPEN_REDB {
            flags
        } else {
            assert_eq!(
                uqa_config_set_compression(config, UQA_COMPRESSION_NONE, 0),
                UQA_OK
            );
            flags | UQA_OPEN_SQLITE_AUTO
        };
        engine = fail_then_open(path, flags, config);
        assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
        assert_eq!(
            uqa_engine_execute(engine, c"SELECT n FROM kept".as_ptr(), result),
            UQA_OK
        );
        let mut value = ptr::null();
        let mut number = 0;
        assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut value), UQA_OK);
        assert_eq!(uqa_value_get_int64(value, &raw mut number), UQA_OK);
        assert_eq!(number, 17);
        assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
    }
    assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
}

#[test]
fn failed_engine_publication_closes_every_provider_and_container_configuration() {
    unsafe {
        open_configuration(UQA_OPEN_MEMORY, false, UQA_COMPRESSION_NONE);
        open_configuration(UQA_OPEN_REDB, false, UQA_COMPRESSION_NONE);
        for flags in [UQA_OPEN_SQLITE_NATIVE, UQA_OPEN_SQLITE_KV] {
            for encrypted in [false, true] {
                for compression in [
                    UQA_COMPRESSION_NONE,
                    UQA_COMPRESSION_LZ4,
                    UQA_COMPRESSION_ZSTD,
                ] {
                    open_configuration(flags, encrypted, compression);
                }
            }
        }
    }
}

#[test]
fn failed_sibling_and_role_sessions_release_shared_native_owners() {
    for flags in PROVIDERS
        .into_iter()
        .filter(|flags| *flags != UQA_OPEN_MEMORY)
    {
        let db = Database::new(flags);
        db.sql(c"CREATE ROLE ffi_reader LOGIN");
        for user in [false, true] {
            let mut sibling = ptr::null_mut();
            unsafe {
                let open = |out| {
                    if user {
                        uqa_engine_open_session_for_user(db.engine, c"ffi_reader".as_ptr(), out)
                    } else {
                        uqa_engine_open_session(db.engine, out)
                    }
                };
                let bytes = db.bytes();
                assert_eq!(
                    target::reject::<UQAEngine, _>(|| open(&raw mut sibling)),
                    UQA_ERROR_OUT_OF_MEMORY
                );
                assert!(sibling.is_null());
                assert_eq!(db.bytes(), bytes);
                assert_eq!(open(&raw mut sibling), UQA_OK);
                let mut result = ptr::null_mut();
                assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
                assert_eq!(
                    uqa_engine_execute(sibling, c"SELECT current_user".as_ptr(), result),
                    UQA_OK
                );
                let mut value = ptr::null();
                let mut text = UQAStringView::default();
                assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut value), UQA_OK);
                assert_eq!(uqa_value_get_text(value, &raw mut text), UQA_OK);
                assert_eq!(
                    std::slice::from_raw_parts(text.data.cast::<u8>(), text.size),
                    if user {
                        b"ffi_reader".as_slice()
                    } else {
                        b"uqa".as_slice()
                    }
                );
                assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
                assert_eq!(uqa_engine_close(&raw mut sibling), UQA_OK);
            }
        }
        assert_eq!(db.integer(c"SELECT 23"), 23);
    }
}

#[test]
fn contextual_value_allocation_precedes_domain_effects_and_preserves_transactions() {
    for flags in PROVIDERS {
        for transaction in [false, true] {
            let db = Database::new(flags);
            db.sql(c"CREATE TABLE input_audit(n integer); CREATE FUNCTION input_check(p integer) RETURNS boolean LANGUAGE plpgsql AS $$ BEGIN INSERT INTO input_audit VALUES (p); RETURN true; END $$; CREATE DOMAIN input_domain AS integer CHECK (input_check(VALUE))");
            unsafe {
                if transaction {
                    assert_eq!(uqa_transaction_begin(db.engine), UQA_OK);
                }
                let bytes = db.bytes();
                let mut value = ptr::null_mut();
                assert_eq!(
                    target::reject::<UQAValue, _>(|| uqa_value_create_from_text(
                        db.engine,
                        c"input_domain".as_ptr(),
                        c"17".as_ptr(),
                        &raw mut value
                    )),
                    UQA_ERROR_OUT_OF_MEMORY
                );
                assert!(value.is_null());
                assert_eq!(db.bytes(), bytes);
                assert_eq!(
                    db.state(),
                    if transaction {
                        UQA_TRANSACTION_ACTIVE
                    } else {
                        UQA_TRANSACTION_IDLE
                    }
                );
                assert_eq!(db.integer(c"SELECT count(*) FROM input_audit"), 0);
                assert_eq!(
                    uqa_value_create_from_text(
                        db.engine,
                        c"input_domain".as_ptr(),
                        c"17".as_ptr(),
                        &raw mut value
                    ),
                    UQA_OK
                );
                assert_eq!(db.integer(c"SELECT n FROM input_audit"), 17);
                assert_eq!(uqa_value_destroy(&raw mut value), UQA_OK);
                if transaction {
                    assert_eq!(uqa_transaction_rollback(db.engine), UQA_OK);
                    assert_eq!(db.integer(c"SELECT count(*) FROM input_audit"), 0);
                }
            }
        }
    }
}

#[test]
fn failed_prepared_cursor_and_cancel_handles_release_their_allowances() {
    for flags in PROVIDERS {
        let db = Database::new(flags);
        let bytes = db.bytes();
        unsafe {
            let mut statement = ptr::null_mut();
            assert_eq!(
                target::reject::<UQAStatement, _>(|| uqa_statement_prepare(
                    db.engine,
                    c"SELECT 17".as_ptr(),
                    &raw mut statement
                )),
                UQA_ERROR_OUT_OF_MEMORY
            );
            assert!(statement.is_null());
            assert_eq!(db.bytes(), bytes);
            assert_eq!(
                uqa_statement_prepare(db.engine, c"SELECT 17".as_ptr(), &raw mut statement),
                UQA_OK
            );
            assert_eq!(uqa_statement_finalize(&raw mut statement), UQA_OK);
            assert_eq!(db.bytes(), bytes);
            let mut cursor = ptr::null_mut();
            assert_eq!(
                target::reject::<UQACursor, _>(|| uqa_cursor_open(
                    db.engine,
                    c"SELECT 17".as_ptr(),
                    &raw mut cursor
                )),
                UQA_ERROR_OUT_OF_MEMORY
            );
            assert!(cursor.is_null());
            assert_eq!(db.bytes(), bytes);
            assert_eq!(
                uqa_cursor_open(db.engine, c"SELECT 17".as_ptr(), &raw mut cursor),
                UQA_OK
            );
            assert_eq!(uqa_cursor_close(&raw mut cursor), UQA_OK);
            assert_eq!(db.bytes(), bytes);
            let mut cancel = ptr::null_mut();
            assert_eq!(
                target::reject::<UQACancelHandle, _>(|| uqa_cancel_handle_create(
                    db.engine,
                    &raw mut cancel
                )),
                UQA_ERROR_OUT_OF_MEMORY
            );
            assert!(cancel.is_null());
            assert_eq!(db.bytes(), bytes);
            assert_eq!(uqa_cancel_handle_create(db.engine, &raw mut cancel), UQA_OK);
            assert_eq!(uqa_cancel_handle_destroy(&raw mut cancel), UQA_OK);
        }
        assert_eq!(db.integer(c"SELECT 17"), 17);
    }
}
