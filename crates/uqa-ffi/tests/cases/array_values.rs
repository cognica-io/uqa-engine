//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed array children preserve their original allowance and immutable concurrent views.

#![allow(unsafe_code)]

use std::{
    ffi::CString,
    ptr,
    sync::{Arc, Barrier},
};
use uqa_ffi::*;

#[test]
fn child_admission_preserves_existing_views_and_recovers_on_every_provider() {
    for flags in [
        UQA_OPEN_MEMORY,
        UQA_OPEN_SQLITE_NATIVE,
        UQA_OPEN_SQLITE_KV,
        UQA_OPEN_REDB,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = CString::new(directory.path().join("arrays.db").to_str().unwrap()).unwrap();
        unsafe {
            let mut config = ptr::null_mut();
            let mut engine = ptr::null_mut();
            let mut result = ptr::null_mut();
            let mut array = ptr::null();
            let mut first = ptr::null();
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
            assert_eq!(uqa_engine_execute(engine, c"CREATE SEQUENCE array_admission_calls; SELECT nextval('array_admission_calls'); CREATE DOMAIN array_admission_element AS integer CHECK (nextval('array_admission_calls') > 0)".as_ptr(), result), UQA_OK);
            assert_eq!(
                uqa_engine_execute(engine, c"SELECT ARRAY[1,2,3]".as_ptr(), result),
                UQA_OK
            );
            assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut array), UQA_OK);
            assert_eq!(uqa_value_get_element(array, 0, &raw mut first), UQA_OK);
            let mut retained = Vec::new();
            loop {
                let mut copy = ptr::null_mut();
                let status = uqa_value_create_copy(first, &raw mut copy);
                if status == UQA_ERROR_LIMIT {
                    assert!(copy.is_null());
                    break;
                }
                assert_eq!(status, UQA_OK);
                retained.push(copy);
                assert!(retained.len() < 256);
            }
            assert!(!retained.is_empty());
            let mut second = first;
            assert_eq!(
                uqa_value_get_element(array, 1, &raw mut second),
                UQA_ERROR_LIMIT
            );
            assert!(second.is_null());
            assert_eq!(uqa_value_get_element(array, 0, &raw mut second), UQA_OK);
            assert_eq!(first, second);
            let mut number = 0;
            assert_eq!(uqa_value_get_int32(first, &raw mut number), UQA_OK);
            assert_eq!(number, 1);
            reject_constructor(engine, first);
            for mut copy in retained {
                assert_eq!(uqa_value_destroy(&raw mut copy), UQA_OK);
            }
            assert_eq!(uqa_value_get_element(array, 1, &raw mut second), UQA_OK);
            assert_eq!(uqa_value_get_int32(second, &raw mut number), UQA_OK);
            assert_eq!(number, 2);
            let mut again = ptr::null();
            assert_eq!(uqa_value_get_element(array, 1, &raw mut again), UQA_OK);
            assert_eq!(second, again);
            cancellation(engine, array);
            assert_eq!(
                uqa_engine_execute(
                    engine,
                    c"SELECT currval('array_admission_calls')".as_ptr(),
                    result
                ),
                UQA_OK
            );
            let mut count = 0;
            assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut first), UQA_OK);
            assert_eq!(uqa_value_get_int64(first, &raw mut count), UQA_OK);
            assert_eq!(count, 1);
            assert_eq!(uqa_transaction_rollback(engine), UQA_OK);
            assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
            assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        }
    }
}

fn reject_constructor(engine: *mut UQAEngine, first: *const UQAValue) {
    unsafe {
        let dimension = UQAArrayDimension {
            length: 1,
            lower_bound: 1,
        };
        let mut constructed = ptr::null_mut();
        assert_eq!(uqa_transaction_begin(engine), UQA_OK);
        assert_eq!(
            uqa_value_create_array(
                engine,
                c"array_admission_element[]".as_ptr(),
                &raw const dimension,
                1,
                &raw const first,
                1,
                &raw mut constructed
            ),
            UQA_ERROR_LIMIT
        );
        assert!(constructed.is_null());
        let mut state = UQA_TRANSACTION_IDLE;
        assert_eq!(uqa_transaction_get_state(engine, &raw mut state), UQA_OK);
        assert_eq!(state, UQA_TRANSACTION_ACTIVE);
    }
}

#[test]
fn competing_array_observers_preserve_admission_and_stable_views() {
    unsafe {
        let mut engine = ptr::null_mut();
        let mut value = ptr::null_mut();
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
            uqa_value_create_from_text(
                engine,
                c"integer[]".as_ptr(),
                c"[-2:-1]={7,9}".as_ptr(),
                &raw mut value
            ),
            UQA_OK
        );
        let address = value as usize;
        let barrier = Arc::new(Barrier::new(4));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    observe_concurrently(address)
                })
            })
            .collect();
        let pointers: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        let mut shape = UQAArrayShape::default();
        let mut element = ptr::null();
        let mut number = 0;
        assert_eq!(uqa_value_get_array_shape(value, &raw mut shape), UQA_OK);
        assert_eq!(uqa_value_get_element(value, 1, &raw mut element), UQA_OK);
        assert_eq!(uqa_value_get_int32(element, &raw mut number), UQA_OK);
        assert_eq!(number, 9);
        for (dimensions, child) in pointers {
            if let Some(dimensions) = dimensions {
                assert_eq!(dimensions, shape.dimensions as usize);
            }
            if let Some(child) = child {
                assert_eq!(child, element as usize);
            }
        }
        assert_eq!(uqa_value_destroy(&raw mut value), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
    }
}

fn observe_concurrently(address: usize) -> (Option<usize>, Option<usize>) {
    let mut dimensions = None;
    let mut child = None;
    unsafe {
        for _ in 0..32 {
            let mut shape = UQAArrayShape {
                dimensions: ptr::dangling(),
                rank: 99,
                element_count: 99,
            };
            let mut element = address as *const UQAValue;
            match uqa_value_get_array_shape(address as *const _, &raw mut shape) {
                UQA_OK => {
                    assert_eq!((shape.rank, shape.element_count), (1, 2));
                    assert_eq!((*shape.dimensions).lower_bound, -2);
                    assert_eq!(
                        *dimensions.get_or_insert(shape.dimensions as usize),
                        shape.dimensions as usize
                    );
                }
                UQA_ERROR_BUSY => assert!(
                    shape.dimensions.is_null() && shape.rank == 0 && shape.element_count == 0
                ),
                status => panic!("unexpected array observation status {status}"),
            }
            match uqa_value_get_element(address as *const _, 1, &raw mut element) {
                UQA_OK => assert_eq!(*child.get_or_insert(element as usize), element as usize),
                UQA_ERROR_BUSY => assert!(element.is_null()),
                status => panic!("unexpected array child status {status}"),
            }
        }
    }
    (dimensions, child)
}

fn cancellation(engine: *mut UQAEngine, array: *const UQAValue) {
    unsafe {
        let mut cancel = ptr::null_mut();
        let mut shape = UQAArrayShape::default();
        let mut element = ptr::null();
        assert_eq!(uqa_value_get_array_shape(array, &raw mut shape), UQA_OK);
        let dimensions = shape.dimensions;
        assert_eq!(uqa_cancel_handle_create(engine, &raw mut cancel), UQA_OK);
        assert_eq!(uqa_cancel_handle_request(cancel), UQA_OK);
        assert_eq!(
            uqa_value_get_array_shape(array, &raw mut shape),
            UQA_ERROR_SQL
        );
        assert!(shape.dimensions.is_null() && shape.rank == 0 && shape.element_count == 0);
        assert_eq!(
            uqa_value_get_element(array, 0, &raw mut element),
            UQA_ERROR_SQL
        );
        assert!(element.is_null());
        let mut error = ptr::null();
        let mut state = UQAStringView::default();
        assert_eq!(uqa_error_get_last(&raw mut error), UQA_OK);
        assert_eq!(uqa_error_get_sqlstate(error, &raw mut state), UQA_OK);
        assert_eq!(
            std::slice::from_raw_parts(state.data.cast::<u8>(), state.size),
            b"57014"
        );
        assert_eq!(uqa_engine_reset_cancellation(engine), UQA_OK);
        assert_eq!(uqa_value_get_array_shape(array, &raw mut shape), UQA_OK);
        assert_eq!(shape.dimensions, dimensions);
        assert_eq!(uqa_value_get_element(array, 0, &raw mut element), UQA_OK);
        assert_eq!(uqa_cancel_handle_destroy(&raw mut cancel), UQA_OK);
    }
}
