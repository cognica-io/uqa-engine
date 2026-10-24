//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fallible C prepared ownership and parameter replacement across native registration.

#![allow(unsafe_code)]

use std::{ffi::CString, ptr};
use uqa_ffi::{
    uqa_config_create, uqa_config_destroy, uqa_config_set_ffi_memory_limit, uqa_engine_close,
    uqa_engine_execute, uqa_engine_open_memory, uqa_result_create, uqa_result_destroy,
    uqa_result_get_value, uqa_statement_bind, uqa_statement_execute, uqa_statement_finalize,
    uqa_statement_prepare, uqa_value_create_int64, uqa_value_create_text_n, uqa_value_destroy,
    uqa_value_get_int64, UQA_ERROR_LIMIT, UQA_OK,
};

#[test]
fn failed_prepared_admission_removes_native_definition_and_binding_failure_keeps_old_value() {
    let wide = CString::new(format!(
        "SELECT {}",
        (1..=100)
            .map(|index| format!("${index}::integer"))
            .collect::<Vec<_>>()
            .join(",")
    ))
    .unwrap();
    unsafe {
        let mut config = ptr::null_mut();
        let mut engine = ptr::null_mut();
        let mut statement = ptr::null_mut();
        let mut result = ptr::null_mut();
        let mut value = ptr::null_mut();
        let mut view = ptr::null();
        let mut integer = 99;
        assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
        assert_eq!(uqa_config_set_ffi_memory_limit(config, 4096), UQA_OK);
        assert_eq!(uqa_engine_open_memory(config, &raw mut engine), UQA_OK);
        assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
        assert_eq!(
            uqa_statement_prepare(engine, wide.as_ptr(), &raw mut statement),
            UQA_ERROR_LIMIT
        );
        assert!(statement.is_null());
        assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
        assert_eq!(
            uqa_engine_execute(
                engine,
                c"SELECT count(*) FROM pg_prepared_statements".as_ptr(),
                result
            ),
            UQA_OK
        );
        assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut view), UQA_OK);
        assert_eq!(uqa_value_get_int64(view, &raw mut integer), UQA_OK);
        assert_eq!(integer, 0);
        assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
        assert_eq!(
            uqa_statement_prepare(engine, c"SELECT $1::bigint".as_ptr(), &raw mut statement),
            UQA_OK
        );
        assert_eq!(uqa_value_create_int64(42, &raw mut value), UQA_OK);
        assert_eq!(uqa_statement_bind(statement, 1, value), UQA_OK);
        assert_eq!(uqa_value_destroy(&raw mut value), UQA_OK);
        let text = vec![b'x'; 50_000];
        assert_eq!(
            uqa_value_create_text_n(text.as_ptr().cast(), text.len(), &raw mut value),
            UQA_OK
        );
        assert_eq!(uqa_statement_bind(statement, 1, value), UQA_ERROR_LIMIT);
        assert_eq!(uqa_value_destroy(&raw mut value), UQA_OK);
        assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
        assert_eq!(uqa_statement_execute(statement, result), UQA_OK);
        assert_eq!(uqa_statement_finalize(&raw mut statement), UQA_OK);
        assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut view), UQA_OK);
        assert_eq!(uqa_value_get_int64(view, &raw mut integer), UQA_OK);
        assert_eq!(integer, 42);
        assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
    }
}
