//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Standard Arrow C Data Interface layouts and feature-independent entry points.

#[cfg(not(feature = "arrow-export"))]
use crate::abi::UQA_ERROR_UNSUPPORTED;
use crate::{boundary, cursor::UQACursor, error::UQAError, result::UQAResult};
use std::ffi::{c_char, c_void};

#[cfg(feature = "arrow-export")]
pub(crate) mod export;

pub const UQA_ARROW_LOSSLESS: u32 = 0;
pub const UQA_ARROW_SQL_TEXT: u32 = 1;

/// The standard Arrow schema layout; names in the C header retain Arrow's guard.
#[repr(C)]
#[derive(Default)]
pub struct UQAArrowSchema {
    pub format: *const c_char,
    pub name: *const c_char,
    pub metadata: *const c_char,
    pub flags: i64,
    pub n_children: i64,
    pub children: *mut *mut Self,
    pub dictionary: *mut Self,
    pub release: Option<unsafe extern "C" fn(*mut Self)>,
    pub private_data: *mut c_void,
}

/// The standard Arrow array layout, independent of the optional Rust producer.
#[repr(C)]
#[derive(Default)]
pub struct UQAArrowArray {
    pub length: i64,
    pub null_count: i64,
    pub offset: i64,
    pub n_buffers: i64,
    pub n_children: i64,
    pub buffers: *mut *const c_void,
    pub children: *mut *mut Self,
    pub dictionary: *mut Self,
    pub release: Option<unsafe extern "C" fn(*mut Self)>,
    pub private_data: *mut c_void,
}

// Both caller-owned structs must be aligned, disjoint and initialized. Checking
// release before clearing preserves live objects on a rejected repeated export.
#[allow(unsafe_code)]
unsafe fn outputs<'a>(
    schema: *mut UQAArrowSchema,
    array: *mut UQAArrowArray,
    policy: u32,
) -> boundary::Result<(&'a mut UQAArrowSchema, &'a mut UQAArrowArray)> {
    let schema = boundary::input_mut(schema)?;
    let array = boundary::input_mut(array)?;
    if schema.release.is_some() || array.release.is_some() {
        return Err(UQAError::state(
            "Arrow output is still owned; release it before reuse",
        ));
    }
    *schema = UQAArrowSchema::default();
    *array = UQAArrowArray::default();
    if !matches!(policy, UQA_ARROW_LOSSLESS | UQA_ARROW_SQL_TEXT) {
        return Err(UQAError::invalid("unknown Arrow export policy"));
    }
    Ok((schema, array))
}

ffi_fn!(uqa_result_export_arrow(result: *mut UQAResult, policy: u32, out_schema: *mut UQAArrowSchema, out_array: *mut UQAArrowArray) {
    let (schema, array) = outputs(out_schema, out_array, policy)?;
    let result = boundary::input(result)?;
    #[cfg(feature = "arrow-export")]
    { export::result(result, policy, schema, array) }
    #[cfg(not(feature = "arrow-export"))]
    {
        let _ = (result, schema, array);
        Err(UQAError::new(UQA_ERROR_UNSUPPORTED, "Arrow export was not enabled in this library"))
    }
});

ffi_fn!(uqa_cursor_fetch_arrow(cursor: *mut UQACursor, max_rows: u32, policy: u32, out_schema: *mut UQAArrowSchema, out_array: *mut UQAArrowArray) {
    let (schema, array) = outputs(out_schema, out_array, policy)?;
    let cursor = boundary::input(cursor)?;
    if max_rows == 0 { return Err(UQAError::invalid("Arrow fetch requires at least one row")); }
    #[cfg(feature = "arrow-export")]
    { cursor.fetch_arrow(max_rows, policy, schema, array) }
    #[cfg(not(feature = "arrow-export"))]
    {
        let _ = (cursor, schema, array);
        Err(UQAError::new(UQA_ERROR_UNSUPPORTED, "Arrow export was not enabled in this library"))
    }
});
