//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native XML admission participates in the contextual input transaction owner.

#![allow(unsafe_code)]

use crate::*;
use std::{ffi::CString, ptr};

fn open(flags: u32, path: &CString) -> (*mut UQAEngine, *mut UQAResult) {
    unsafe {
        let mut config = ptr::null_mut();
        let mut engine = ptr::null_mut();
        let mut result = ptr::null_mut();
        assert_eq!(uqa_config_create(&raw mut config), UQA_OK);
        assert_eq!(uqa_config_set_ffi_memory_limit(config, 64 << 10), UQA_OK);
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
        assert_eq!(uqa_engine_execute(engine,
            c"CREATE TABLE xml_audit(n integer); CREATE FUNCTION xml_admitted(p xml) RETURNS boolean LANGUAGE plpgsql AS $$ BEGIN INSERT INTO xml_audit VALUES (1); RETURN true; END $$; CREATE DOMAIN admitted_xml AS xml CHECK (xml_admitted(VALUE))".as_ptr(), result), UQA_OK);
        (engine, result)
    }
}

fn rejected_xml(engine: *mut UQAEngine, input: &CString) {
    unsafe {
        let mut value = ptr::null_mut();
        assert_eq!(
            uqa_value_create_from_text(
                engine,
                c"admitted_xml".as_ptr(),
                input.as_ptr(),
                &raw mut value
            ),
            UQA_ERROR_SQL
        );
        assert!(value.is_null());
        let mut error = ptr::null();
        let mut state = UQAStringView::default();
        assert_eq!(uqa_error_get_last(&raw mut error), UQA_OK);
        assert_eq!(uqa_error_get_sqlstate(error, &raw mut state), UQA_OK);
        assert_eq!(
            std::slice::from_raw_parts(state.data.cast::<u8>(), state.size),
            b"53200"
        );
    }
}

#[test]
fn xml_parser_admission_preserves_values_and_recovers_native_transactions_on_every_provider() {
    let large = CString::new("<n/>".repeat(2000)).unwrap();
    for flags in [
        UQA_OPEN_MEMORY,
        UQA_OPEN_SQLITE_NATIVE,
        UQA_OPEN_SQLITE_KV,
        UQA_OPEN_REDB,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = CString::new(directory.path().join("xml.db").to_str().unwrap()).unwrap();
        let (mut engine, mut result) = open(flags, &path);
        unsafe {
            let mut original = ptr::null_mut();
            assert_eq!(
                uqa_value_create_from_text(
                    engine,
                    c"admitted_xml".as_ptr(),
                    c"<a/>".as_ptr(),
                    &raw mut original
                ),
                UQA_OK
            );
            let mut text = ptr::null_mut();
            // The same input fits as text; native parser allocations exhaust the allowance.
            assert_eq!(
                uqa_value_create_from_text(engine, c"text".as_ptr(), large.as_ptr(), &raw mut text),
                UQA_OK
            );
            assert_eq!(uqa_value_destroy(&raw mut text), UQA_OK);
            for _ in 0..8 {
                rejected_xml(engine, &large);
            }
            let mut view = UQAStringView::default();
            assert_eq!(uqa_value_get_xml(original, &raw mut view), UQA_OK);
            assert_eq!(
                std::slice::from_raw_parts(view.data.cast::<u8>(), view.size),
                b"<a/>"
            );
            assert_eq!(
                uqa_engine_execute(engine, c"BEGIN; SAVEPOINT xml_keep".as_ptr(), result),
                UQA_OK
            );
            rejected_xml(engine, &large);
            let mut state = 0;
            assert_eq!(uqa_transaction_get_state(engine, &raw mut state), UQA_OK);
            assert_eq!(state, UQA_TRANSACTION_FAILED);
            assert_eq!(
                uqa_engine_execute(engine, c"ROLLBACK TO xml_keep".as_ptr(), result),
                UQA_OK
            );
            let mut value = ptr::null_mut();
            assert_eq!(
                uqa_value_create_from_text(
                    engine,
                    c"admitted_xml".as_ptr(),
                    c"<b/>".as_ptr(),
                    &raw mut value
                ),
                UQA_OK
            );
            assert_eq!(uqa_value_destroy(&raw mut value), UQA_OK);
            assert_eq!(
                uqa_engine_execute(
                    engine,
                    c"COMMIT; SELECT count(*) FROM xml_audit".as_ptr(),
                    result
                ),
                UQA_OK
            );
            let mut cell = ptr::null();
            let mut count = 0;
            assert_eq!(uqa_result_get_value(result, 0, 0, &raw mut cell), UQA_OK);
            assert_eq!(uqa_value_get_int64(cell, &raw mut count), UQA_OK);
            assert_eq!(count, 2);
            assert_eq!(uqa_value_destroy(&raw mut original), UQA_OK);
            assert_eq!(uqa_result_destroy(&raw mut result), UQA_OK);
            assert_eq!(uqa_engine_close(&raw mut engine), UQA_OK);
        }
    }
}
