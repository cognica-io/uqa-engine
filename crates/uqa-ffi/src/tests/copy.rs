//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Callbacks use live stack data; the worker joins before either Engine can be released.
#![allow(unsafe_code)]

use crate::{
    uqa_engine_close, uqa_engine_copy_to, uqa_engine_execute, uqa_engine_open, uqa_error_get_last,
    uqa_error_get_status, uqa_result_create, uqa_result_destroy, UQAEngine, UQAStatus, UQA_DONE,
    UQA_ERROR_BUSY, UQA_ERROR_STATE, UQA_OK, UQA_OPEN_MEMORY,
};
use std::{ffi::c_void, ptr};

struct Invocation {
    other: *mut UQAEngine,
    visited: bool,
    statuses: [UQAStatus; 4],
}

unsafe extern "C" fn write(
    user_data: *mut c_void,
    _: *const u8,
    size: usize,
    written: *mut usize,
) -> UQAStatus {
    let invocation = &mut *user_data.cast::<Invocation>();
    if !invocation.visited {
        invocation.visited = true;
        let mut result = ptr::null_mut();
        let status = uqa_result_create(&raw mut result);
        if status != UQA_OK {
            return status;
        }
        invocation.statuses[0] = uqa_engine_execute(invocation.other, c"SELECT 1".as_ptr(), result);
        let other = invocation.other as usize;
        let worker = std::thread::spawn(move || {
            let mut result = ptr::null_mut();
            let status = uqa_result_create(&raw mut result);
            if status != UQA_OK {
                return status;
            }
            let status = uqa_engine_execute(other as *mut UQAEngine, c"SELECT 42".as_ptr(), result);
            let cleanup = uqa_result_destroy(&raw mut result);
            if status == UQA_OK {
                cleanup
            } else {
                status
            }
        });
        invocation.statuses[1] = worker.join().unwrap_or(UQA_ERROR_STATE);
        let mut error = ptr::null();
        invocation.statuses[2] = uqa_error_get_last(&raw mut error);
        invocation.statuses[3] = if error.is_null() {
            UQA_ERROR_STATE
        } else {
            let mut status = UQA_OK;
            let inspected = uqa_error_get_status(error, &raw mut status);
            if inspected == UQA_OK {
                status
            } else {
                inspected
            }
        };
        let status = uqa_result_destroy(&raw mut result);
        if status != UQA_OK {
            return status;
        }
    }
    *written = size;
    UQA_OK
}

#[test]
fn callback_admission_and_diagnostics_are_local_to_the_invoking_thread() {
    unsafe {
        let mut engine = ptr::null_mut();
        let mut other = ptr::null_mut();
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
            uqa_engine_open(
                ptr::null(),
                UQA_OPEN_MEMORY,
                ptr::null_mut(),
                &raw mut other
            ),
            UQA_OK
        );
        let mut invocation = Invocation {
            other,
            visited: false,
            statuses: [99; 4],
        };
        let mut rows = 0;
        assert_eq!(
            uqa_engine_copy_to(
                engine,
                c"COPY (SELECT 1) TO STDOUT".as_ptr(),
                Some(write),
                None,
                (&raw mut invocation).cast(),
                &raw mut rows
            ),
            UQA_OK
        );
        assert_eq!(rows, 1);
        assert!(invocation.visited);
        assert_eq!(
            invocation.statuses,
            [UQA_ERROR_BUSY, UQA_OK, UQA_OK, UQA_ERROR_BUSY]
        );
        let mut error = ptr::null();
        assert_eq!(uqa_error_get_last(&raw mut error), UQA_DONE);
        assert!(error.is_null());
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut other), UQA_OK);
    }
}
