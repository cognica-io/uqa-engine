//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Tests supply valid allocated C storage and exercise the exported unsafe boundary.
#![allow(unsafe_code)]

use std::ptr;
use uqa_ffi::{
    uqa_config_create, uqa_config_destroy, uqa_config_get_encryption_key,
    uqa_config_get_ffi_memory_limit, uqa_config_set_encryption_key_n,
    uqa_config_set_ffi_memory_limit, uqa_error_get_last, uqa_error_get_message, UQAStringView,
    UQA_DONE, UQA_ERROR_INVALID_ARGUMENT, UQA_ERROR_RANGE, UQA_OK,
};

#[test]
fn config_pairs_preserve_previous_values_and_managed_diagnostics() {
    unsafe {
        let mut config = ptr::null_mut();
        assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
        assert_eq!(uqa_config_set_ffi_memory_limit(config, 12345), UQA_OK);
        let mut bytes = 0;
        assert_eq!(
            uqa_config_get_ffi_memory_limit(config, &raw mut bytes),
            UQA_OK
        );
        assert_eq!(bytes, 12345);
        assert_eq!(
            uqa_config_set_ffi_memory_limit(config, 0),
            UQA_ERROR_INVALID_ARGUMENT
        );
        let mut error = ptr::null();
        assert_eq!(uqa_error_get_last(&raw mut error), UQA_OK);
        let mut message = UQAStringView::default();
        assert_eq!(uqa_error_get_message(error, &raw mut message), UQA_OK);
        assert!(message.size > 0);
        assert_eq!(
            uqa_error_get_message(error, ptr::null_mut()),
            UQA_ERROR_INVALID_ARGUMENT
        );
        assert_eq!(uqa_error_get_message(error, &raw mut message), UQA_OK);
        assert_eq!(
            uqa_config_get_ffi_memory_limit(config, &raw mut bytes),
            UQA_OK
        );
        assert_eq!(bytes, 12345);
        assert_eq!(uqa_error_get_last(&raw mut error), UQA_DONE);
        assert!(error.is_null());
        assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
        assert!(config.is_null());
        assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
        assert_eq!(uqa_config_destroy(ptr::null_mut()), UQA_OK);
    }
}

#[test]
fn string_spans_and_borrowed_self_assignment_are_checked() {
    unsafe {
        let mut config = ptr::null_mut();
        assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
        let key = b"key without terminator";
        assert_eq!(
            uqa_config_set_encryption_key_n(config, key.as_ptr().cast(), key.len()),
            UQA_OK
        );
        let mut view = UQAStringView::default();
        assert_eq!(uqa_config_get_encryption_key(config, &raw mut view), UQA_OK);
        assert_eq!(
            uqa_config_set_encryption_key_n(config, view.data, view.size),
            UQA_OK
        );
        assert_eq!(
            uqa_config_set_encryption_key_n(config, ptr::null(), usize::MAX),
            UQA_ERROR_RANGE
        );
        assert_eq!(
            uqa_config_set_encryption_key_n(config, b"a\0b".as_ptr().cast(), 3),
            UQA_ERROR_INVALID_ARGUMENT
        );
        assert_eq!(uqa_config_get_encryption_key(config, &raw mut view), UQA_OK);
        assert_eq!(
            std::slice::from_raw_parts(view.data.cast::<u8>(), view.size),
            key
        );
        assert_eq!(uqa_config_destroy(&raw mut config), UQA_OK);
    }
}
