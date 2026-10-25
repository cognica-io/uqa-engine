//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field descriptor failures preserve prior views and never run constructor effects.

#![allow(unsafe_code)]

use std::{ffi::CString, fmt::Write as _, ptr};
use uqa_ffi::*;

#[test]
fn field_admission_and_cancellation_keep_existing_descriptors_on_every_provider() {
    for flags in [
        UQA_OPEN_MEMORY,
        UQA_OPEN_SQLITE_NATIVE,
        UQA_OPEN_SQLITE_KV,
        UQA_OPEN_REDB,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = CString::new(directory.path().join("records.db").to_str().unwrap()).unwrap();
        unsafe {
            let (mut engine, mut result) = open(flags, &path);
            let mut value = ptr::null_mut();
            assert_eq!(
                uqa_value_create_typed_null(engine, c"admission_record".as_ptr(), &raw mut value),
                UQA_OK
            );
            let mut ty = ptr::null();
            let mut first = UQAFieldDescriptor::default();
            let mut next = UQAFieldDescriptor::default();
            assert_eq!(uqa_value_get_type(value, &raw mut ty), UQA_OK);
            assert_eq!(uqa_type_get_field(ty, 0, &raw mut first), UQA_OK);
            let copies = fill_allowance(value);
            assert_eq!(uqa_type_get_field(ty, 1, &raw mut next), UQA_ERROR_LIMIT);
            assert!(next.ty.is_null() && next.name.data.is_null());
            assert_eq!(uqa_type_get_field(ty, 0, &raw mut next), UQA_OK);
            assert_eq!(first.ty, next.ty);
            reject_constructor(engine);
            for mut copy in copies {
                assert_eq!(uqa_value_destroy(&raw mut copy), UQA_OK);
            }
            assert_eq!(uqa_type_get_field(ty, 1, &raw mut next), UQA_OK);
            let second = next.ty;
            let mut cancel = ptr::null_mut();
            assert_eq!(uqa_cancel_handle_create(engine, &raw mut cancel), UQA_OK);
            assert_eq!(uqa_cancel_handle_request(cancel), UQA_OK);
            assert_eq!(uqa_type_get_field(ty, 1, &raw mut next), UQA_ERROR_SQL);
            assert!(next.ty.is_null());
            assert_eq!(uqa_engine_reset_cancellation(engine), UQA_OK);
            assert_eq!(uqa_type_get_field(ty, 1, &raw mut next), UQA_OK);
            assert_eq!(next.ty, second);
            assert_eq!(uqa_cancel_handle_destroy(&raw mut cancel), UQA_OK);
            assert_eq!(
                uqa_engine_execute(
                    engine,
                    c"SELECT currval('record_admission_calls')".as_ptr(),
                    result
                ),
                UQA_OK
            );
            let mut cell = ptr::null();
            let mut number = 0;
            assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut cell), UQA_OK);
            assert_eq!(uqa_value_get_int64(cell, &raw mut number), UQA_OK);
            assert_eq!(number, 1);
            assert_eq!(uqa_value_destroy(&raw mut value), UQA_OK);
            assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
            assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        }
    }
}

fn reject_constructor(engine: *mut UQAEngine) {
    unsafe {
        let mut scalar = ptr::null_mut();
        let mut constructed = ptr::null_mut();
        assert_eq!(uqa_value_create_int32(1, &raw mut scalar), UQA_OK);
        let fields = [scalar.cast_const(); 2];
        assert_eq!(uqa_transaction_begin(engine), UQA_OK);
        assert_eq!(
            uqa_value_create_record(
                engine,
                c"admission_record".as_ptr(),
                fields.as_ptr(),
                2,
                &raw mut constructed
            ),
            UQA_ERROR_LIMIT
        );
        assert!(constructed.is_null());
        let mut state = UQA_TRANSACTION_IDLE;
        assert_eq!(uqa_transaction_get_state(engine, &raw mut state), UQA_OK);
        assert_eq!(state, UQA_TRANSACTION_ACTIVE);
        assert_eq!(uqa_transaction_rollback(engine), UQA_OK);
        assert_eq!(uqa_value_destroy(&raw mut scalar), UQA_OK);
    }
}

fn open(flags: UQAOpenFlags, path: &CString) -> (*mut UQAEngine, *mut UQAResult) {
    unsafe {
        let mut config = ptr::null_mut();
        let mut engine = ptr::null_mut();
        let mut result = ptr::null_mut();
        assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
        assert_eq!(uqa_config_set_ffi_memory_limit(config, 65536), UQA_OK);
        assert_eq!(
            uqa_engine_open(
                if flags == UQA_OPEN_MEMORY {
                    ptr::null()
                } else {
                    path.as_ptr()
                },
                flags,
                config,
                &raw mut engine
            ),
            UQA_OK
        );
        assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
        assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
        let mut setup = String::from(
            "CREATE SEQUENCE record_admission_calls; SELECT nextval('record_admission_calls'); CREATE DOMAIN d0 AS integer CHECK(nextval('record_admission_calls') > 0);",
        );
        // The complete inherited type is larger than one retained NULL value; admitting it requires space released by the held owners.
        for index in 1..=8 {
            write!(setup, "CREATE DOMAIN d{index} AS d{};", index - 1).unwrap();
        }
        setup.push_str("CREATE TYPE admission_record AS (a integer, b d8)");
        let setup = CString::new(setup).unwrap();
        assert_eq!(uqa_engine_execute(engine, setup.as_ptr(), result), UQA_OK);
        (engine, result)
    }
}

fn fill_allowance(value: *const UQAValue) -> Vec<*mut UQAValue> {
    let mut copies = Vec::new();
    unsafe {
        loop {
            let mut copy = ptr::null_mut();
            match uqa_value_create_copy(value, &raw mut copy) {
                UQA_OK => copies.push(copy),
                UQA_ERROR_LIMIT => {
                    assert!(copy.is_null());
                    return copies;
                }
                status => panic!("unexpected copy status {status}"),
            }
            assert!(copies.len() < 256);
        }
    }
}

#[test]
fn field_value_admission_and_cancellation_preserve_published_children_on_every_provider() {
    for flags in [
        UQA_OPEN_MEMORY,
        UQA_OPEN_SQLITE_NATIVE,
        UQA_OPEN_SQLITE_KV,
        UQA_OPEN_REDB,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path =
            CString::new(directory.path().join("field-values.db").to_str().unwrap()).unwrap();
        unsafe {
            let (mut engine, mut result) = open(flags, &path);
            let (mut value, mut cancel) = (ptr::null_mut(), ptr::null_mut());
            let (mut first, mut second, mut again) = (ptr::null(), ptr::null(), ptr::null());
            assert_eq!(
                uqa_value_create_from_text(
                    engine,
                    c"admission_record".as_ptr(),
                    c"(1,2)".as_ptr(),
                    &raw mut value
                ),
                UQA_OK
            );
            assert_eq!(uqa_value_get_field(value, 0, &raw mut first), UQA_OK);
            let mut number = 0;
            assert_eq!(uqa_value_get_int32(first, &raw mut number), UQA_OK);
            assert_eq!(number, 1);
            let copies = fill_allowance(first);
            // Native field observation reports its resource failure with SQLSTATE; C wrapper admission uses UQA_ERROR_LIMIT.
            assert_eq!(
                uqa_value_get_field(value, 1, &raw mut second),
                UQA_ERROR_SQL
            );
            let mut error = ptr::null();
            let mut state = UQAStringView::default();
            assert_eq!(uqa_error_get_last(&raw mut error), UQA_OK);
            assert_eq!(uqa_error_get_sqlstate(error, &raw mut state), UQA_OK);
            assert_eq!(
                std::slice::from_raw_parts(state.data.cast::<u8>(), state.size),
                b"53200"
            );
            assert!(second.is_null());
            assert_eq!(uqa_value_get_field(value, 0, &raw mut again), UQA_OK);
            assert_eq!(first, again);
            assert_eq!(uqa_value_get_int32(first, &raw mut number), UQA_OK);
            assert_eq!(number, 1);
            for mut copy in copies {
                assert_eq!(uqa_value_destroy(&raw mut copy), UQA_OK);
            }
            assert_eq!(uqa_value_get_field(value, 1, &raw mut second), UQA_OK);
            assert_eq!(uqa_value_get_int32(second, &raw mut number), UQA_OK);
            assert_eq!(number, 2);
            assert_eq!(uqa_cancel_handle_create(engine, &raw mut cancel), UQA_OK);
            assert_eq!(uqa_cancel_handle_request(cancel), UQA_OK);
            assert_eq!(uqa_value_get_field(value, 1, &raw mut again), UQA_ERROR_SQL);
            assert!(again.is_null());
            let mut count = 9;
            assert_eq!(
                uqa_value_get_field_count(value, &raw mut count),
                UQA_ERROR_SQL
            );
            assert_eq!(count, 0);
            assert_eq!(uqa_engine_reset_cancellation(engine), UQA_OK);
            assert_eq!(uqa_value_get_field(value, 1, &raw mut again), UQA_OK);
            assert_eq!(second, again);
            assert_eq!(
                uqa_engine_execute(
                    engine,
                    c"SELECT currval('record_admission_calls')".as_ptr(),
                    result
                ),
                UQA_OK
            );
            assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut again), UQA_OK);
            let mut calls = 0;
            assert_eq!(uqa_value_get_int64(again, &raw mut calls), UQA_OK);
            assert_eq!(calls, 2);
            assert_eq!(uqa_cancel_handle_destroy(&raw mut cancel), UQA_OK);
            assert_eq!(uqa_value_destroy(&raw mut value), UQA_OK);
            assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
            assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        }
    }
}
