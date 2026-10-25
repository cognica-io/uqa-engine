//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Failed result and live catalog view publication keeps the original public owners intact.

use super::{
    fixtures::{Database, PROVIDERS},
    target,
};
use crate::*;
use std::ptr;

unsafe fn integer(value: *const UQAValue) -> i64 {
    let mut number = 0;
    assert_eq!(uqa_value_get_int64(value, &raw mut number), UQA_OK);
    number
}

#[test]
fn result_and_each_batch_slot_are_allocated_before_native_effects() {
    for flags in PROVIDERS {
        let db = Database::new(flags);
        db.sql(c"CREATE TABLE allocation_effects(n integer); SELECT 42");
        unsafe {
            let mut original = ptr::null();
            assert_eq!(
                uqa_result_get_value(db.result, 0, 0, &raw mut original),
                UQA_OK
            );
            let bytes = db.bytes();
            assert_eq!(
                target::reject_named("uqa::result::ResultData", 0, || uqa_engine_execute(
                    db.engine,
                    c"INSERT INTO allocation_effects VALUES (1)".as_ptr(),
                    db.result
                )),
                UQA_ERROR_OUT_OF_MEMORY
            );
            assert_eq!(integer(original), 42);
            assert_eq!(db.bytes(), bytes);
            assert_eq!(db.integer(c"SELECT count(*) FROM allocation_effects"), 0);
            let mut batch = ptr::null_mut();
            assert_eq!(uqa_batch_result_create(&raw mut batch), UQA_OK);
            let seed = UQASQLRequest {
                sql: c"SELECT 23".as_ptr(),
                params: ptr::null(),
                param_count: 0,
            };
            assert_eq!(
                uqa_engine_execute_batch(db.engine, &raw const seed, 1, batch),
                UQA_OK
            );
            let requests = [
                UQASQLRequest {
                    sql: c"INSERT INTO allocation_effects VALUES (2)".as_ptr(),
                    params: ptr::null(),
                    param_count: 0,
                },
                UQASQLRequest {
                    sql: c"INSERT INTO allocation_effects VALUES (3)".as_ptr(),
                    params: ptr::null(),
                    param_count: 0,
                },
            ];
            for skip in 0..2 {
                let bytes = db.bytes();
                assert_eq!(
                    target::reject_named("uqa::result::ResultData", skip, || {
                        uqa_engine_execute_batch(
                            db.engine,
                            requests.as_ptr(),
                            requests.len(),
                            batch,
                        )
                    }),
                    UQA_ERROR_OUT_OF_MEMORY
                );
                assert_eq!(db.bytes(), bytes);
                let mut count = 0;
                assert_eq!(uqa_batch_result_get_count(batch, &raw mut count), UQA_OK);
                assert_eq!(count, 1);
                assert_eq!(db.integer(c"SELECT count(*) FROM allocation_effects"), 0);
            }
            assert_eq!(uqa_batch_result_take(batch, 0, db.result), UQA_OK);
            assert_eq!(
                uqa_result_get_value(db.result, 0, 0, &raw mut original),
                UQA_OK
            );
            assert_eq!(integer(original), 23);
            assert_eq!(uqa_batch_result_destroy(&raw mut batch), UQA_OK);
        }
    }
}

#[test]
fn live_composite_type_and_field_allocation_failures_preserve_prior_views() {
    for flags in PROVIDERS {
        let db = Database::new(flags);
        db.sql(c"CREATE TYPE allocation_pair AS (n integer, label text); SELECT ROW(17, 'kept')::allocation_pair");
        unsafe {
            let mut ty = ptr::null();
            let bytes = db.bytes();
            assert_eq!(
                target::reject::<UQAType, _>(|| uqa_result_get_column_type(
                    db.result,
                    0,
                    &raw mut ty
                )),
                UQA_ERROR_OUT_OF_MEMORY
            );
            assert!(ty.is_null());
            assert_eq!(db.bytes(), bytes);
            assert_eq!(
                uqa_result_get_column_type(db.result, 0, &raw mut ty),
                UQA_OK
            );
            let mut first = UQAFieldDescriptor::default();
            assert_eq!(uqa_type_get_field(ty, 0, &raw mut first), UQA_OK);
            for name in [
                std::any::type_name::<UQAType>(),
                "uqa::types::fields::Field",
            ] {
                let bytes = db.bytes();
                let mut second = UQAFieldDescriptor::default();
                assert_eq!(
                    target::reject_named(name, 0, || uqa_type_get_field(ty, 1, &raw mut second)),
                    UQA_ERROR_OUT_OF_MEMORY
                );
                assert!(second.ty.is_null() && second.name.data.is_null());
                assert_eq!(db.bytes(), bytes);
                assert_eq!(
                    std::slice::from_raw_parts(first.name.data.cast::<u8>(), first.name.size),
                    b"n"
                );
                let mut kind = 0;
                assert_eq!(uqa_type_get_kind(first.ty, &raw mut kind), UQA_OK);
                assert_eq!(kind, UQA_TYPE_INT32);
            }
            let mut second = UQAFieldDescriptor::default();
            assert_eq!(uqa_type_get_field(ty, 1, &raw mut second), UQA_OK);
            let mut record = ptr::null();
            assert_eq!(
                uqa_result_get_value(db.result, 0, 0, &raw mut record),
                UQA_OK
            );
            for name in [
                "uqa::value::records::Generation",
                std::any::type_name::<UQAValue>(),
            ] {
                let bytes = db.bytes();
                let mut child = ptr::null();
                assert_eq!(
                    target::reject_named(name, 0, || uqa_value_get_field(
                        record,
                        0,
                        &raw mut child
                    )),
                    UQA_ERROR_OUT_OF_MEMORY
                );
                assert!(child.is_null());
                assert_eq!(db.bytes(), bytes);
            }
            let mut child = ptr::null();
            assert_eq!(uqa_value_get_field(record, 0, &raw mut child), UQA_OK);
            assert_eq!(integer(child), 17);
            let bytes = db.bytes();
            let mut other = ptr::null();
            assert_eq!(
                target::reject::<UQAValue, _>(|| uqa_value_get_field(record, 1, &raw mut other)),
                UQA_ERROR_OUT_OF_MEMORY
            );
            assert!(other.is_null());
            assert_eq!(db.bytes(), bytes);
            assert_eq!(integer(child), 17);
            assert_eq!(uqa_value_get_field(record, 1, &raw mut other), UQA_OK);
        }
    }
}
