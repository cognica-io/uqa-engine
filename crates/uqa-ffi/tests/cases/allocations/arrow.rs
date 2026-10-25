//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Partial Arrow wrapping releases every child and keeps publication atomic.

use super::{
    fixtures::{Database, PROVIDERS},
    target,
};
use std::ptr;
use uqa_ffi::*;

const OWNERS: [&str; 2] = [
    "uqa_ffi::arrow::export::SchemaOwner",
    "uqa_ffi::arrow::export::ArrayOwner",
];

fn unpublished(schema: &UQAArrowSchema, array: &UQAArrowArray) {
    assert!(schema.release.is_none() && schema.private_data.is_null() && schema.children.is_null());
    assert!(array.release.is_none() && array.private_data.is_null() && array.children.is_null());
}

#[test]
fn every_arrow_child_and_root_allocation_failure_releases_partial_ownership() {
    for flags in PROVIDERS {
        let db = Database::new(flags);
        db.sql(c"SELECT 17::integer AS n, 'kept'::text AS s, NULL::bigint AS absent");
        let bytes = db.bytes();
        for owner in OWNERS {
            for skip in 0..4 {
                let mut schema = UQAArrowSchema::default();
                let mut array = UQAArrowArray::default();
                unsafe {
                    assert_eq!(
                        target::reject_named(owner, skip, || uqa_result_export_arrow(
                            db.result,
                            UQA_ARROW_LOSSLESS,
                            &raw mut schema,
                            &raw mut array
                        )),
                        UQA_ERROR_OUT_OF_MEMORY
                    );
                    unpublished(&schema, &array);
                    assert_eq!(
                        db.bytes(),
                        bytes,
                        "partial {owner} ownership leaked at {skip}"
                    );
                    assert_eq!(
                        uqa_result_export_arrow(
                            db.result,
                            UQA_ARROW_LOSSLESS,
                            &raw mut schema,
                            &raw mut array
                        ),
                        UQA_OK
                    );
                    assert_eq!(
                        (schema.n_children, array.n_children, array.length),
                        (3, 3, 1)
                    );
                    let integer = &**array.children;
                    assert_eq!((*integer.buffers.add(1)).cast::<i32>().read(), 17);
                    schema.release.unwrap()(&raw mut schema);
                    array.release.unwrap()(&raw mut array);
                    assert_eq!(db.bytes(), bytes);
                }
            }
        }
    }
}

#[test]
fn cursor_arrow_allocation_failure_is_terminal_and_releases_unpublished_output() {
    for flags in PROVIDERS {
        let db = Database::new(flags);
        for owner in OWNERS {
            for skip in 0..4 {
                let mut cursor = ptr::null_mut();
                let mut schema = UQAArrowSchema::default();
                let mut array = UQAArrowArray::default();
                let bytes = db.bytes();
                unsafe {
                    assert_eq!(uqa_cursor_open(db.engine, c"SELECT n::integer, 'kept'::text, NULL::bigint FROM generate_series(1, 5) AS g(n)".as_ptr(), &raw mut cursor), UQA_OK);
                    assert_eq!(
                        target::reject_named(owner, skip, || uqa_cursor_fetch_arrow(
                            cursor,
                            2,
                            UQA_ARROW_LOSSLESS,
                            &raw mut schema,
                            &raw mut array
                        )),
                        UQA_ERROR_OUT_OF_MEMORY
                    );
                    unpublished(&schema, &array);
                    assert_eq!(
                        uqa_cursor_fetch_arrow(
                            cursor,
                            1,
                            UQA_ARROW_LOSSLESS,
                            &raw mut schema,
                            &raw mut array
                        ),
                        UQA_ERROR_OUT_OF_MEMORY
                    );
                    assert_eq!(
                        uqa_cursor_fetch(cursor, 1, db.result),
                        UQA_ERROR_OUT_OF_MEMORY
                    );
                    unpublished(&schema, &array);
                    assert_eq!(uqa_cursor_close(&raw mut cursor), UQA_OK);
                    assert_eq!(db.bytes(), bytes);
                }
            }
        }
    }
}
