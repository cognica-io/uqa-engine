//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar observation retains its output under the original session allowance.

#![allow(unsafe_code)]

use std::ptr;
use uqa_ffi::{
    uqa_config_create, uqa_config_destroy, uqa_config_set_ffi_memory_limit, uqa_engine_close,
    uqa_engine_execute, uqa_engine_open, uqa_result_create, uqa_result_destroy,
    uqa_result_get_value, uqa_value_create_copy, uqa_value_destroy, uqa_value_get_is_null,
    uqa_value_get_numeric, uqa_value_get_numeric_scale, uqa_value_get_text, UQAStringView,
    UQA_ERROR_BUSY, UQA_ERROR_LIMIT, UQA_OK, UQA_OPEN_MEMORY,
};

#[test]
fn contextual_handle_allocation_precedes_domain_input_and_transaction_effects() {
    use uqa_ffi::{
        uqa_transaction_begin, uqa_transaction_get_state, uqa_transaction_rollback,
        uqa_value_create_from_text, uqa_value_create_typed_null, uqa_value_get_int64,
        UQA_TRANSACTION_ACTIVE,
    };
    unsafe {
        let mut config = ptr::null_mut();
        let mut engine = ptr::null_mut();
        let mut result = ptr::null_mut();
        let mut original = ptr::null_mut();
        assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
        assert_eq!(uqa_config_set_ffi_memory_limit(config, 4096), UQA_OK);
        assert_eq!(
            uqa_engine_open(ptr::null(), UQA_OPEN_MEMORY, config, &raw mut engine),
            UQA_OK
        );
        assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
        assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
        assert_eq!(uqa_engine_execute(engine,
            c"CREATE TABLE input_audit(n integer); CREATE FUNCTION input_check(p integer) RETURNS boolean LANGUAGE plpgsql AS $$ BEGIN INSERT INTO input_audit VALUES (p); RETURN true; END $$; CREATE DOMAIN input_domain AS integer CHECK (input_check(VALUE))".as_ptr(),
            result,
        ), UQA_OK);
        assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
        assert_eq!(
            uqa_value_create_typed_null(engine, c"integer".as_ptr(), &raw mut original),
            UQA_OK
        );
        let mut retained = Vec::new();
        loop {
            let mut copy = ptr::null_mut();
            let status = uqa_value_create_copy(original, &raw mut copy);
            if status == UQA_ERROR_LIMIT {
                assert!(copy.is_null());
                break;
            }
            assert_eq!(status, UQA_OK);
            retained.push(copy);
            assert!(retained.len() < 64);
        }
        assert!(!retained.is_empty());
        assert_eq!(uqa_transaction_begin(engine), UQA_OK);
        let mut rejected = ptr::null_mut();
        assert_eq!(
            uqa_value_create_from_text(
                engine,
                c"input_domain".as_ptr(),
                c"7".as_ptr(),
                &raw mut rejected
            ),
            UQA_ERROR_LIMIT
        );
        assert!(rejected.is_null());
        let mut state = 0;
        assert_eq!(uqa_transaction_get_state(engine, &raw mut state), UQA_OK);
        assert_eq!(state, UQA_TRANSACTION_ACTIVE);
        for mut value in retained {
            assert_eq!(uqa_value_destroy(&raw mut value), UQA_OK);
        }
        assert_eq!(uqa_value_destroy(&raw mut original), UQA_OK);
        assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
        assert_eq!(
            uqa_engine_execute(engine, c"SELECT count(*) FROM input_audit".as_ptr(), result),
            UQA_OK
        );
        let mut cell = ptr::null();
        let mut count = -1;
        assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut cell), UQA_OK);
        assert_eq!(uqa_value_get_int64(cell, &raw mut count), UQA_OK);
        assert_eq!(count, 0);
        assert_eq!(uqa_transaction_rollback(engine), UQA_OK);
        assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
    }
}

#[test]
fn numeric_text_allocation_failure_keeps_the_value_and_other_retained_payloads() {
    unsafe {
        let mut config = ptr::null_mut();
        let mut engine = ptr::null_mut();
        let mut result = ptr::null_mut();
        let mut numeric = ptr::null();
        let mut text_value = ptr::null();
        let mut text = UQAStringView::default();
        assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
        assert_eq!(uqa_config_set_ffi_memory_limit(config, 64 * 1024), UQA_OK);
        assert_eq!(
            uqa_engine_open(ptr::null(), UQA_OPEN_MEMORY, config, &raw mut engine),
            UQA_OK
        );
        assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
        assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
        assert_eq!(
            uqa_engine_execute(
                engine,
                c"SELECT 1e4096::numeric, repeat('x', 4096)".as_ptr(),
                result
            ),
            UQA_OK
        );
        assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut numeric), UQA_OK);
        assert_eq!(
            uqa_result_get_value(result, 0, 1, &raw mut text_value),
            UQA_OK
        );
        let mut retained = Vec::new();
        loop {
            let mut copy = ptr::null_mut();
            let status = uqa_value_create_copy(text_value, &raw mut copy);
            if status == UQA_ERROR_LIMIT {
                assert!(copy.is_null());
                break;
            }
            assert_eq!(status, UQA_OK);
            retained.push(copy);
            assert!(
                retained.len() < 64,
                "retention must consume the configured allowance"
            );
        }
        assert!(!retained.is_empty());
        assert_eq!(
            uqa_value_get_numeric(numeric, &raw mut text),
            UQA_ERROR_LIMIT
        );
        assert!(text.data.is_null());
        assert_eq!(text.size, 0);
        let mut is_null = 1;
        let mut scale = 99;
        assert_eq!(uqa_value_get_is_null(numeric, &raw mut is_null), UQA_OK);
        assert_eq!(is_null, 0);
        assert_eq!(uqa_value_get_numeric_scale(numeric, &raw mut scale), UQA_OK);
        assert_eq!(scale, 0);
        assert_eq!(uqa_value_get_text(retained[0], &raw mut text), UQA_OK);
        assert_eq!(
            std::slice::from_raw_parts(text.data.cast::<u8>(), text.size),
            vec![b'x'; 4096]
        );
        for mut value in retained {
            assert_eq!(uqa_value_destroy(&raw mut value), UQA_OK);
        }
        assert_eq!(uqa_value_get_numeric(numeric, &raw mut text), UQA_OK);
        assert_eq!(text.size, 4097);
        let bytes = std::slice::from_raw_parts(text.data.cast::<u8>(), text.size);
        assert_eq!(bytes[0], b'1');
        assert!(bytes[1..].iter().all(|byte| *byte == b'0'));
        let original_text = text.data;
        assert_eq!(uqa_value_get_numeric(numeric, &raw mut text), UQA_OK);
        assert_eq!(text.data, original_text);
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_ERROR_BUSY);
        assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
    }
}
