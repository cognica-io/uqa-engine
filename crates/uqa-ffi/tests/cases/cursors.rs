//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Tests supply valid C addresses and retain all borrowed storage until the call completes.
#![allow(unsafe_code)]

use std::{ffi::CString, ptr};
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_ffi::{
    uqa_config_create, uqa_config_destroy, uqa_config_set_ffi_memory_limit, uqa_cursor_close,
    uqa_cursor_fetch, uqa_cursor_open, uqa_engine_close, uqa_engine_open, uqa_error_get_completion,
    uqa_error_get_last, uqa_result_create, uqa_result_destroy, uqa_result_get_value,
    uqa_value_get_int64, UQACursor, UQAStatus, UQA_COMPLETION_COMMITTED, UQA_COMPLETION_NONE,
    UQA_DONE, UQA_ERROR_BUSY, UQA_ERROR_LIMIT, UQA_OK, UQA_OPEN_MEMORY, UQA_OPEN_SQLITE_NATIVE,
};

#[test]
fn cursor_admission_precedes_evaluation_and_descriptor_failure_reports_completed_effects() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("cursor-admission.db");
    let path = CString::new(database.to_str().unwrap()).unwrap();
    let native = Engine::open(&database).unwrap();
    native.sql("CREATE SEQUENCE observed START 1", &[]).unwrap();

    let columns = (0..64)
        .map(|index| format!("{index} AS c{index}"))
        .collect::<Vec<_>>();
    let wide = CString::new(format!(
        "SELECT nextval('observed'), {}",
        columns.join(", ")
    ))
    .unwrap();
    let narrow = c"SELECT nextval('observed')";
    for (limit, sql, completion, next) in [
        (1, narrow, UQA_COMPLETION_NONE, 1),
        (4096, wide.as_c_str(), UQA_COMPLETION_COMMITTED, 3),
    ] {
        unsafe {
            let mut config = ptr::null_mut();
            let mut engine = ptr::null_mut();
            let mut cursor = ptr::null_mut();
            let mut error = ptr::null();
            let mut actual_completion = 99;
            assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
            assert_eq!(uqa_config_set_ffi_memory_limit(config, limit), UQA_OK);
            assert_eq!(
                uqa_engine_open(
                    path.as_ptr(),
                    UQA_OPEN_SQLITE_NATIVE,
                    config,
                    &raw mut engine
                ),
                UQA_OK
            );
            assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
            assert_eq!(
                uqa_cursor_open(engine, sql.as_ptr(), &raw mut cursor),
                UQA_ERROR_LIMIT
            );
            assert!(cursor.is_null());
            assert_eq!(uqa_error_get_last(&raw mut error), UQA_OK);
            assert_eq!(
                uqa_error_get_completion(error, &raw mut actual_completion),
                UQA_OK
            );
            assert_eq!(actual_completion, completion);
            assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        }
        let result = native.sql("SELECT nextval('observed')", &[]).unwrap();
        assert_eq!(result.value_at(0, 0), Some(&Value::Int(next)));
    }
    native.close().unwrap();
}

fn admitted(mut operation: impl FnMut() -> UQAStatus) -> UQAStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let status = operation();
        if status != UQA_ERROR_BUSY {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session admission remained busy"
        );
        std::thread::yield_now();
    }
}

#[test]
fn concurrent_fetches_admit_one_caller_and_deliver_each_row_once() {
    unsafe {
        let mut engine = ptr::null_mut();
        let mut cursor = ptr::null_mut();
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
            uqa_cursor_open(
                engine,
                c"SELECT n FROM generate_series(1, 500) AS s(n) ORDER BY n".as_ptr(),
                &raw mut cursor,
            ),
            UQA_OK
        );
        let address = cursor as usize;
        let barrier = std::sync::Barrier::new(2);
        let mut values = std::thread::scope(|scope| {
            let workers = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        // C permits concurrent valid calls while destruction remains synchronized.
                        let cursor = address as *mut UQACursor;
                        let mut result = ptr::null_mut();
                        assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
                        let mut values = Vec::new();
                        barrier.wait();
                        loop {
                            let status = admitted(|| uqa_cursor_fetch(cursor, 1, result));
                            if status == UQA_DONE {
                                break;
                            }
                            assert_eq!(status, UQA_OK);
                            let mut value = ptr::null();
                            let mut number = 0;
                            assert_eq!(
                                admitted(|| uqa_result_get_value(result, 0, 0, &raw mut value)),
                                UQA_OK
                            );
                            assert_eq!(
                                admitted(|| uqa_value_get_int64(value, &raw mut number)),
                                UQA_OK
                            );
                            values.push(number);
                        }
                        assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
                        values
                    })
                })
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .flat_map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        values.sort_unstable();
        assert_eq!(values, (1..=500).collect::<Vec<_>>());
        assert_eq!(uqa_cursor_close(&raw mut cursor), UQA_OK);
        assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
    }
}
