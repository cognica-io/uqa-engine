//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Packed observation keeps one admitted cache and leaves the source usable after a rejected allocation.

#![allow(unsafe_code)]

use crate::*;
use std::{ffi::CString, ptr};

#[test]
fn vector_observation_releases_failed_admission_on_every_provider() {
    observation(false);
}

#[test]
fn tensor_observation_releases_failed_admission_on_every_provider() {
    observation(true);
}

fn observation(tensor: bool) {
    let row = format!("ARRAY[{}]", vec!["1.0"; 64].join(","));
    let expression = if tensor {
        format!("ARRAY[{}]::tensor(64)", vec![row; 32].join(","))
    } else {
        format!("ARRAY[{}]::vector(2048)", vec!["1.0"; 2048].join(","))
    };
    let query = CString::new(format!("SELECT {expression}, repeat('x', 4096)")).unwrap();
    for flags in [
        UQA_OPEN_MEMORY,
        UQA_OPEN_SQLITE_NATIVE,
        UQA_OPEN_SQLITE_KV,
        UQA_OPEN_REDB,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = CString::new(directory.path().join("dense.db").to_str().unwrap()).unwrap();
        unsafe {
            let mut config = ptr::null_mut();
            let mut engine = ptr::null_mut();
            let mut result = ptr::null_mut();
            let mut value = ptr::null();
            let mut text_value = ptr::null();
            assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
            assert_eq!(uqa_config_set_ffi_memory_limit(config, 64 * 1024), UQA_OK);
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
            assert_eq!(uqa_engine_execute(engine, query.as_ptr(), result), UQA_OK);
            assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut value), UQA_OK);
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
                assert!(retained.len() < 32);
            }
            assert!(!retained.is_empty());
            let mut vector_view = UQAVectorView::default();
            let mut tensor_view = UQATensorView::default();
            let status = if tensor {
                uqa_value_get_tensor_f32(value, &raw mut tensor_view)
            } else {
                uqa_value_get_vector_f32(value, &raw mut vector_view)
            };
            assert_eq!(status, UQA_ERROR_SQL);
            let mut error = ptr::null();
            let mut state = UQAStringView::default();
            assert_eq!(uqa_error_get_last(&raw mut error), UQA_OK);
            assert_eq!(uqa_error_get_sqlstate(error, &raw mut state), UQA_OK);
            assert_eq!(
                std::slice::from_raw_parts(state.data.cast::<u8>(), state.size),
                b"53200"
            );
            assert!(vector_view.data.is_null() && tensor_view.data.is_null());
            let mut text = UQAStringView::default();
            assert_eq!(uqa_value_get_text(retained[0], &raw mut text), UQA_OK);
            assert_eq!(
                std::slice::from_raw_parts(text.data.cast::<u8>(), text.size),
                vec![b'x'; 4096]
            );
            for mut copy in retained {
                assert_eq!(uqa_value_destroy(&raw mut copy), UQA_OK);
            }
            let first = assert_observation(value, tensor);
            assert_eq!(first, assert_observation(value, tensor));
            assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
            assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        }
    }
}

fn assert_observation(value: *const UQAValue, tensor: bool) -> *const f32 {
    unsafe {
        let data = if tensor {
            let mut view = UQATensorView::default();
            assert_eq!(uqa_value_get_tensor_f32(value, &raw mut view), UQA_OK);
            assert_eq!((view.row_count, view.dimensions), (32, 64));
            view.data
        } else {
            let mut view = UQAVectorView::default();
            assert_eq!(uqa_value_get_vector_f32(value, &raw mut view), UQA_OK);
            assert_eq!(view.dimensions, 2048);
            view.data
        };
        assert_eq!(std::slice::from_raw_parts(data, 2048), vec![1.0; 2048]);
        data
    }
}
