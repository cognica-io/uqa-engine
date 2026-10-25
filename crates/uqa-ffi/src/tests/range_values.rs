//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained range observations fail within their original allowance and retry without constructor effects.

#![allow(unsafe_code)]

use crate::*;
use std::{ffi::CString, ptr};

#[test]
fn range_admission_failure_cancellation_and_retry_preserve_native_state_on_all_providers() {
    for flags in [
        UQA_OPEN_MEMORY,
        UQA_OPEN_SQLITE_NATIVE,
        UQA_OPEN_SQLITE_KV,
        UQA_OPEN_REDB,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = CString::new(directory.path().join("ranges.db").to_str().unwrap()).unwrap();
        unsafe {
            let (mut engine, mut result) = open(flags, &path);
            let mut multi = ptr::null_mut();
            let mut seed = ptr::null_mut();
            let text = CString::new(format!(
                "{{{}}}",
                (0..32)
                    .map(|i| format!("[{},{}]", i * 3, i * 3 + 1))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
            .unwrap();
            assert_eq!(
                uqa_value_create_from_text(
                    engine,
                    c"int4multirange".as_ptr(),
                    text.as_ptr(),
                    &raw mut multi
                ),
                UQA_OK
            );
            assert_eq!(
                uqa_value_create_typed_null(engine, c"integer".as_ptr(), &raw mut seed),
                UQA_OK
            );
            let mut owners = Vec::new();
            loop {
                let mut copy = ptr::null_mut();
                let status = uqa_value_create_copy(seed, &raw mut copy);
                if status == UQA_ERROR_LIMIT {
                    assert!(copy.is_null());
                    break;
                }
                assert_eq!(status, UQA_OK);
                owners.push(copy);
                assert!(owners.len() < 256);
            }
            let mut count = 99;
            assert_eq!(
                uqa_value_get_range_count(multi, &raw mut count),
                UQA_ERROR_LIMIT
            );
            assert_eq!(count, 0);
            reject_constructor(engine);
            for mut copy in owners {
                assert_eq!(uqa_value_destroy(&raw mut copy), UQA_OK);
            }
            assert_eq!(uqa_value_get_range_count(multi, &raw mut count), UQA_OK);
            assert_eq!(count, 32);
            let mut member = ptr::null();
            assert_eq!(uqa_value_get_range(multi, 0, &raw mut member), UQA_OK);
            let mut bounds = UQARangeBounds::default();
            assert_eq!(uqa_value_get_range_bounds(member, &raw mut bounds), UQA_OK);
            let lower = bounds.lower;
            let mut cancel = ptr::null_mut();
            assert_eq!(uqa_cancel_handle_create(engine, &raw mut cancel), UQA_OK);
            assert_eq!(uqa_cancel_handle_request(cancel), UQA_OK);
            assert_eq!(
                uqa_value_get_range_bounds(member, &raw mut bounds),
                UQA_ERROR_SQL
            );
            assert!(bounds.lower.is_null() && bounds.upper.is_null() && bounds.flags == 0);
            assert_eq!(uqa_engine_reset_cancellation(engine), UQA_OK);
            assert_eq!(uqa_value_get_range_bounds(member, &raw mut bounds), UQA_OK);
            assert_eq!(bounds.lower, lower);
            assert_eq!(uqa_cancel_handle_destroy(&raw mut cancel), UQA_OK);
            assert_eq!(uqa_value_destroy(&raw mut seed), UQA_OK);
            assert_eq!(uqa_value_destroy(&raw mut multi), UQA_OK);
            assert_eq!(
                uqa_engine_execute(
                    engine,
                    c"SELECT currval('range_admission_calls')".as_ptr(),
                    result
                ),
                UQA_OK
            );
            let mut cell = ptr::null();
            let mut number = 0;
            assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut cell), UQA_OK);
            assert_eq!(uqa_value_get_int64(cell, &raw mut number), UQA_OK);
            assert_eq!(number, 1);
            assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
            assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        }
    }
}

fn reject_constructor(engine: *mut UQAEngine) {
    unsafe {
        let mut scalar = ptr::null_mut();
        let mut range = ptr::null_mut();
        assert_eq!(uqa_value_create_int32(1, &raw mut scalar), UQA_OK);
        assert_eq!(uqa_transaction_begin(engine), UQA_OK);
        assert_eq!(
            uqa_value_create_range(
                engine,
                c"counted_range".as_ptr(),
                scalar,
                scalar,
                6,
                &raw mut range
            ),
            UQA_ERROR_LIMIT
        );
        assert!(range.is_null());
        let mut state = UQA_TRANSACTION_IDLE;
        assert_eq!(uqa_transaction_get_state(engine, &raw mut state), UQA_OK);
        assert_eq!(state, UQA_TRANSACTION_ACTIVE);
        assert_eq!(uqa_transaction_rollback(engine), UQA_OK);
        assert_eq!(uqa_value_destroy(&raw mut scalar), UQA_OK);
    }
}

fn open(flags: u32, path: &CString) -> (*mut UQAEngine, *mut UQAResult) {
    unsafe {
        let (mut config, mut engine, mut result) =
            (ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
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
        assert_eq!(uqa_engine_execute(engine, c"CREATE SEQUENCE range_admission_calls; SELECT nextval('range_admission_calls'); CREATE DOMAIN counted_range AS int4range CHECK(nextval('range_admission_calls') > 0)".as_ptr(), result), UQA_OK);
        (engine, result)
    }
}
