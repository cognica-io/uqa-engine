//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Real provider handles and observable allowance/transaction state for native failures.

#![allow(unsafe_code)]

use crate::*;
use std::{
    ffi::{CStr, CString},
    ptr,
};

pub(super) const PROVIDERS: [UQAOpenFlags; 4] = [
    UQA_OPEN_MEMORY,
    UQA_OPEN_SQLITE_NATIVE,
    UQA_OPEN_SQLITE_KV,
    UQA_OPEN_REDB,
];

pub(super) struct Database {
    pub engine: *mut UQAEngine,
    pub result: *mut UQAResult,
    flags: UQAOpenFlags,
    directory: tempfile::TempDir,
}

impl Database {
    pub fn new(flags: UQAOpenFlags) -> Self {
        Self::with_config(flags, ptr::null_mut())
    }

    pub fn with_config(flags: UQAOpenFlags, config: *mut UQAConfig) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = CString::new(directory.path().join("allocation.db").to_str().unwrap()).unwrap();
        let mut engine = ptr::null_mut();
        let mut result = ptr::null_mut();
        unsafe {
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
            assert_eq!(uqa_result_create(&raw mut result), UQA_OK);
        }
        Self {
            engine,
            result,
            flags,
            directory,
        }
    }

    pub fn sql(&self, sql: &CStr) {
        unsafe {
            assert_eq!(
                uqa_engine_execute(self.engine, sql.as_ptr(), self.result),
                UQA_OK
            );
        }
    }

    pub fn integer(&self, sql: &CStr) -> i64 {
        self.sql(sql);
        self.retained_integer()
    }

    pub fn retained_integer(&self) -> i64 {
        let mut value = ptr::null();
        let mut integer = 0;
        unsafe {
            assert_eq!(
                uqa_result_get_value(self.result, 0, 0, &raw mut value),
                UQA_OK
            );
            assert_eq!(uqa_value_get_int64(value, &raw mut integer), UQA_OK);
        }
        integer
    }

    pub fn bytes(&self) -> usize {
        unsafe { test_support::retained_ffi_bytes(self.engine).unwrap() }
    }

    pub fn state(&self) -> u32 {
        let mut state = u32::MAX;
        unsafe {
            assert_eq!(
                uqa_transaction_get_state(self.engine, &raw mut state),
                UQA_OK
            );
        }
        state
    }

    pub fn reopen(&mut self, config: *mut UQAConfig) {
        let path = CString::new(
            self.directory
                .path()
                .join("allocation.db")
                .to_str()
                .unwrap(),
        )
        .unwrap();
        unsafe {
            assert_eq!(uqa_result_destroy(&raw mut self.result), UQA_OK);
            assert_eq!(uqa_engine_close(&raw mut self.engine), UQA_OK);
            assert_eq!(
                uqa_engine_open(path.as_ptr(), self.flags, config, &raw mut self.engine),
                UQA_OK
            );
            assert_eq!(uqa_result_create(&raw mut self.result), UQA_OK);
        }
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        unsafe {
            let result = uqa_result_destroy(&raw mut self.result);
            let engine = uqa_engine_close(&raw mut self.engine);
            if !std::thread::panicking() {
                assert_eq!(
                    (result, engine),
                    (UQA_OK, UQA_OK),
                    "failed allocation leaked a session lease"
                );
            }
        }
    }
}
