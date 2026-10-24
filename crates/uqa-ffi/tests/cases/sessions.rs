//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Tests provide live C storage and release owned handles through the public ABI.
#![allow(unsafe_code)]

use std::{ffi::CString, ptr};
use uqa_ffi::{
    uqa_config_create, uqa_config_destroy, uqa_config_set_compression,
    uqa_config_set_ffi_memory_limit, uqa_engine_close, uqa_engine_execute,
    uqa_engine_execute_params, uqa_engine_get_capabilities, uqa_engine_open,
    uqa_engine_open_session, uqa_result_create, uqa_result_destroy, uqa_result_get_value,
    uqa_transaction_begin, uqa_transaction_get_state, uqa_transaction_rollback,
    uqa_value_create_copy, uqa_value_get_int64, uqa_value_get_text, UQAStringView,
    UQA_CAP_INDEPENDENT_SESSIONS, UQA_CAP_PERSISTENCE, UQA_COMPRESSION_ZSTD, UQA_ERROR_BUSY,
    UQA_ERROR_INVALID_ARGUMENT, UQA_ERROR_LIMIT, UQA_ERROR_TYPE, UQA_ERROR_UNSUPPORTED, UQA_OK,
    UQA_OPEN_MEMORY, UQA_OPEN_SQLITE_NATIVE, UQA_TRANSACTION_IDLE,
};

#[test]
fn persistent_siblings_share_data_but_own_transactions_and_lifetimes() {
    let directory = tempfile::tempdir().unwrap();
    let path = CString::new(directory.path().join("sessions.db").to_str().unwrap()).unwrap();
    unsafe {
        let mut first = ptr::null_mut();
        let mut second = ptr::null_mut();
        let mut first_result = ptr::null_mut();
        let mut second_result = ptr::null_mut();
        assert_eq!(
            uqa_engine_open(
                path.as_ptr(),
                UQA_OPEN_SQLITE_NATIVE,
                ptr::null_mut(),
                &raw mut first
            ),
            UQA_OK
        );
        assert_eq!(uqa_result_create(&raw mut first_result), UQA_OK);
        assert_eq!(uqa_result_create(&raw mut second_result), UQA_OK);
        assert_eq!(
            uqa_engine_execute(
                first,
                c"CREATE TABLE t(n integer); INSERT INTO t VALUES(7)".as_ptr(),
                first_result
            ),
            UQA_OK
        );
        assert_eq!(uqa_engine_open_session(first, &raw mut second), UQA_OK);
        let mut capabilities = 0;
        assert_eq!(
            uqa_engine_get_capabilities(second, &raw mut capabilities),
            UQA_OK
        );
        assert_ne!(capabilities & UQA_CAP_PERSISTENCE, 0);
        assert_ne!(capabilities & UQA_CAP_INDEPENDENT_SESSIONS, 0);
        assert_eq!(uqa_transaction_begin(first), UQA_OK);
        let mut state = 99;
        assert_eq!(uqa_transaction_get_state(second, &raw mut state), UQA_OK);
        assert_eq!(state, UQA_TRANSACTION_IDLE);
        assert_eq!(uqa_transaction_rollback(first), UQA_OK);
        assert_eq!(uqa_result_destroy(&raw mut first_result), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut first), UQA_OK);
        assert!(first.is_null());
        assert_eq!(
            uqa_engine_execute(second, c"SELECT n FROM t".as_ptr(), second_result),
            UQA_OK
        );
        let mut value = ptr::null();
        let mut number = 0;
        assert_eq!(
            uqa_result_get_value(second_result, 0, 0, &raw mut value),
            UQA_OK
        );
        assert_eq!(uqa_value_get_int64(value, &raw mut number), UQA_OK);
        assert_eq!(number, 7);
        assert_eq!(uqa_engine_close(&raw mut second), UQA_ERROR_BUSY);
        assert_eq!(uqa_result_destroy(&raw mut second_result), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut second), UQA_OK);
    }
}

#[test]
fn memory_sessions_and_inapplicable_storage_options_are_rejected() {
    unsafe {
        let mut config = ptr::null_mut();
        let mut engine = ptr::null_mut();
        let mut sibling = ptr::null_mut();
        assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
        assert_eq!(
            uqa_config_set_compression(config, UQA_COMPRESSION_ZSTD, 3),
            UQA_OK
        );
        assert_eq!(
            uqa_engine_open(ptr::null(), UQA_OPEN_MEMORY, config, &raw mut engine),
            UQA_ERROR_INVALID_ARGUMENT
        );
        assert!(engine.is_null());
        assert_eq!(
            uqa_engine_open(
                ptr::null(),
                UQA_OPEN_MEMORY,
                ptr::null_mut(),
                &raw mut engine
            ),
            UQA_OK
        );
        assert_eq!(
            uqa_engine_open_session(engine, &raw mut sibling),
            UQA_ERROR_UNSUPPORTED
        );
        assert!(sibling.is_null());
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
    }
}

#[test]
fn catalog_values_cannot_rebind_equal_oids_from_a_different_database() {
    unsafe {
        let mut first = ptr::null_mut();
        let mut second = ptr::null_mut();
        let mut source = ptr::null_mut();
        let mut destination = ptr::null_mut();
        assert_eq!(
            uqa_engine_open(
                ptr::null(),
                UQA_OPEN_MEMORY,
                ptr::null_mut(),
                &raw mut first
            ),
            UQA_OK
        );
        assert_eq!(
            uqa_engine_open(
                ptr::null(),
                UQA_OPEN_MEMORY,
                ptr::null_mut(),
                &raw mut second
            ),
            UQA_OK
        );
        assert_eq!(uqa_result_create(&raw mut source), UQA_OK);
        assert_eq!(uqa_result_create(&raw mut destination), UQA_OK);
        assert_eq!(
            uqa_engine_execute(
                first,
                c"CREATE DOMAIN number AS integer; SELECT 7::number".as_ptr(),
                source
            ),
            UQA_OK
        );
        assert_eq!(
            uqa_engine_execute(
                second,
                c"CREATE DOMAIN number AS integer; SELECT 99".as_ptr(),
                destination
            ),
            UQA_OK
        );
        let mut value = ptr::null();
        assert_eq!(uqa_result_get_value(source, 0, 0, &raw mut value), UQA_OK);
        assert_eq!(
            uqa_engine_execute_params(
                second,
                c"SELECT $1".as_ptr(),
                &raw const value,
                1,
                destination
            ),
            UQA_ERROR_TYPE
        );
        let mut retained = ptr::null();
        let mut number = 0;
        assert_eq!(
            uqa_result_get_value(destination, 0, 0, &raw mut retained),
            UQA_OK
        );
        assert_eq!(uqa_value_get_int64(retained, &raw mut number), UQA_OK);
        assert_eq!(number, 99);
        assert_eq!(
            uqa_engine_execute(first, c"SELECT 7::integer".as_ptr(), source),
            UQA_OK
        );
        assert_eq!(uqa_result_get_value(source, 0, 0, &raw mut value), UQA_OK);
        assert_eq!(
            uqa_engine_execute_params(
                second,
                c"SELECT $1 + 1".as_ptr(),
                &raw const value,
                1,
                destination
            ),
            UQA_OK
        );
        assert_eq!(
            uqa_result_get_value(destination, 0, 0, &raw mut retained),
            UQA_OK
        );
        assert_eq!(uqa_value_get_int64(retained, &raw mut number), UQA_OK);
        assert_eq!(number, 8);
        assert_eq!(uqa_result_destroy(&raw mut source), UQA_OK);
        assert_eq!(uqa_result_destroy(&raw mut destination), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut first), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut second), UQA_OK);
    }
}

#[test]
fn result_admission_precedes_sql_and_value_copies_hold_their_allowance() {
    let directory = tempfile::tempdir().unwrap();
    let native_path = directory.path().join("admission.db");
    let path = CString::new(native_path.to_str().unwrap()).unwrap();
    unsafe {
        let mut config = ptr::null_mut();
        let mut engine = ptr::null_mut();
        let mut result = ptr::null_mut();
        assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
        assert_eq!(uqa_config_set_ffi_memory_limit(config, 1), UQA_OK);
        assert_eq!(
            uqa_engine_open(
                path.as_ptr(),
                UQA_OPEN_SQLITE_NATIVE,
                config,
                &raw mut engine
            ),
            UQA_OK
        );
        assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
        assert_eq!(
            uqa_engine_execute(engine, c"CREATE TABLE denied(n integer)".as_ptr(), result),
            UQA_ERROR_LIMIT
        );
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        let native = uqa_engine::Engine::open(&native_path).unwrap();
        let error = native.sql("SELECT * FROM denied", &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P01"));
        native.close().unwrap();
        drop(native);
        assert_eq!(uqa_config_set_ffi_memory_limit(config, 4096), UQA_OK);
        assert_eq!(
            uqa_engine_open(ptr::null(), UQA_OPEN_MEMORY, config, &raw mut engine),
            UQA_OK
        );
        assert_eq!(
            uqa_engine_execute(engine, c"SELECT repeat('x', 50000)".as_ptr(), result),
            UQA_OK
        );
        let mut value = ptr::null();
        let mut copied = ptr::null_mut();
        let mut text = UQAStringView::default();
        assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut value), UQA_OK);
        assert_eq!(uqa_value_get_text(value, &raw mut text), UQA_OK);
        assert_eq!(text.size, 50000);
        assert_eq!(
            uqa_value_create_copy(value, &raw mut copied),
            UQA_ERROR_LIMIT
        );
        assert!(copied.is_null());
        assert_eq!(uqa_value_get_text(value, &raw mut text), UQA_OK);
        assert_eq!(text.size, 50000);
        assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
    }
}
