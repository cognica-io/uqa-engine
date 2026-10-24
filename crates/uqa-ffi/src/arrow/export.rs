//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish independently releasable Arrow owners without retaining a session.

use super::{UQAArrowArray, UQAArrowSchema, UQA_ARROW_LOSSLESS};
use crate::{
    abi::{UQAStatus, UQA_ERROR_LIMIT, UQA_ERROR_UNSUPPORTED, UQA_OK},
    boundary,
    error::UQAError,
    result::{NativeResult, ResultData, UQAResult},
};
use arrow_array::{ffi::FFI_ArrowArray, Array, StructArray};
use arrow_schema::ffi::FFI_ArrowSchema;
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Arc,
};
use uqa_api::arrow_export::{export_arrow, ArrowExportPolicy, ArrowRows};
use uqa_core::memory::Budgeted;

impl ArrowRows for NativeResult {
    fn row_count(&self) -> usize {
        self.row_count()
    }
    fn column_count(&self) -> usize {
        self.column_count()
    }
    fn column_name(&self, column: usize) -> &str {
        self.column_name(column)
    }
    fn column_type(&self, column: usize) -> Option<&uqa_sql::ColumnType> {
        self.column_type(column)
    }
    fn value_at(&self, row: usize, column: usize) -> Option<&uqa_core::Value> {
        self.value_at(row, column)
    }
}

struct SchemaOwner {
    native: FFI_ArrowSchema,
    _memory: Arc<Budgeted<()>>,
}

struct ArrayOwner {
    native: FFI_ArrowArray,
    _memory: Arc<Budgeted<()>>,
}

// Arrow's standard layout is ABI-stable. The native crate and public facade both
// use repr(C); these checks also run in every target/feature compilation.
const _: () = {
    assert!(size_of::<UQAArrowSchema>() == size_of::<FFI_ArrowSchema>());
    assert!(align_of::<UQAArrowSchema>() == align_of::<FFI_ArrowSchema>());
    assert!(size_of::<UQAArrowArray>() == size_of::<FFI_ArrowArray>());
    assert!(align_of::<UQAArrowArray>() == align_of::<FFI_ArrowArray>());
};

// The caller returns one live Arrow owner. Setting release to NULL first makes
// recursive/repeated releases harmless; private data never points at the caller's
// movable top-level struct. No panic may escape a foreign release callback.
#[allow(unsafe_code)]
unsafe extern "C" fn release_schema(schema: *mut UQAArrowSchema) {
    let Some(schema) = schema.as_mut() else {
        return;
    };
    if schema.release.take().is_none() {
        return;
    }
    let owner = std::mem::replace(&mut schema.private_data, std::ptr::null_mut());
    let _ = catch_unwind(AssertUnwindSafe(|| {
        drop(Box::from_raw(owner.cast::<SchemaOwner>()));
    }));
}

// Same standard ownership contract as release_schema; the array's lease is independent.
#[allow(unsafe_code)]
unsafe extern "C" fn release_array(array: *mut UQAArrowArray) {
    let Some(array) = array.as_mut() else {
        return;
    };
    if array.release.take().is_none() {
        return;
    }
    let owner = std::mem::replace(&mut array.private_data, std::ptr::null_mut());
    let _ = catch_unwind(AssertUnwindSafe(|| {
        drop(Box::from_raw(owner.cast::<ArrayOwner>()));
    }));
}

fn conversion_error(error: uqa_sql::SQLError) -> UQAError {
    let unsupported = matches!(error, uqa_sql::SQLError::Unsupported(_));
    let mut error = UQAError::from(error);
    if unsupported {
        error.status = UQA_ERROR_UNSUPPORTED;
    }
    error
}

pub(super) fn result(
    result: &UQAResult,
    policy: u32,
    schema: &mut UQAArrowSchema,
    array: &mut UQAArrowArray,
) -> boundary::Result<UQAStatus> {
    let data = result.data()?;
    let _call = data.session.enter()?;
    publish(data, policy, schema, array)
}

/// The cursor already owns session admission. Conversion failure is returned to
/// its terminal-state owner before any output struct is published.
pub(crate) fn publish(
    data: &ResultData,
    policy: u32,
    schema: &mut UQAArrowSchema,
    array: &mut UQAArrowArray,
) -> boundary::Result<UQAStatus> {
    let control = data.session.control();
    let policy = if policy == UQA_ARROW_LOSSLESS {
        ArrowExportPolicy::Lossless
    } else {
        ArrowExportPolicy::SQLText
    };
    let output = export_arrow(&data.native, policy, Some(&data.session.native), &control)
        .map_err(conversion_error)?;
    // Native FFI owners copy field names/metadata and allocate child/borrowed-buffer
    // tables. Charge those capacities separately from the already-owned Arrow data.
    let mut bytes = 4096usize;
    for field in output.schema().fields() {
        let metadata = field
            .metadata()
            .iter()
            .try_fold(field.name().len(), |total, (key, value)| {
                total.checked_add(key.len())?.checked_add(value.len())
            });
        bytes = metadata
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| n.checked_add(4096))
            .and_then(|n| n.checked_add(bytes))
            .ok_or_else(|| {
                UQAError::new(UQA_ERROR_LIMIT, "Arrow ownership metadata is too large")
            })?;
    }
    let schema_memory = data.session.memory.reserve(bytes)?;
    let array_bytes = output
        .num_columns()
        .checked_add(1)
        .and_then(|columns| columns.checked_mul(512))
        .and_then(|bytes| bytes.checked_add(4096))
        .ok_or_else(|| UQAError::new(UQA_ERROR_LIMIT, "Arrow buffer metadata is too large"))?;
    let mut array_memory = data.session.memory.reserve(array_bytes)?;
    let (output, payload) = output.into_parts();
    array_memory.absorb(payload.expect("session-controlled Arrow output"));
    let mut native_schema = FFI_ArrowSchema::try_from(output.schema().as_ref())
        .map_err(|error| UQAError::display(crate::abi::UQA_ERROR_TYPE, &error))?;
    let mut native_array = FFI_ArrowArray::new(&StructArray::from(output).to_data());
    let schema_memory = Budgeted::new((), schema_memory).into_shared()?;
    let array_memory = Budgeted::new((), array_memory).into_shared()?;
    // Safety: each native producer owns its standard-layout tree. Wrapping every
    // node keeps the allowance with children that a consumer moves out before
    // releasing their parent. Partial wrapping remains releasable by native Drop.
    #[allow(unsafe_code)]
    unsafe {
        wrap_schema(
            std::ptr::from_mut(&mut native_schema).cast(),
            &schema_memory,
        )?;
        wrap_array(std::ptr::from_mut(&mut native_array).cast(), &array_memory)?;
    }
    control.check()?;
    // Publication moves both standard facades only after every allocation succeeds.
    #[allow(unsafe_code)]
    unsafe {
        *schema = std::ptr::read(std::ptr::from_ref(&native_schema).cast::<UQAArrowSchema>());
        *array = std::ptr::read(std::ptr::from_ref(&native_array).cast::<UQAArrowArray>());
    }
    std::mem::forget(native_schema);
    std::mem::forget(native_array);
    Ok(UQA_OK)
}

// The pointers below come only from our native Arrow producer, never arbitrary
// caller input. Nodes contain primitive columns under one struct root. Allocate
// an empty owner first; transferring its original release state cannot then fail.
macro_rules! wrap_node {
    ($wrap:ident, $facade:ty, $native:ty, $owner:ident, $release:ident) => {
        #[allow(unsafe_code)]
        unsafe fn $wrap(node: *mut $facade, memory: &Arc<Budgeted<()>>) -> boundary::Result<()> {
            let node = &mut *node;
            for index in 0..node.n_children as usize {
                $wrap(*node.children.add(index), memory)?;
            }
            if !node.dictionary.is_null() {
                $wrap(node.dictionary, memory)?;
            }
            let mut owner = boundary::boxed($owner {
                native: <$native>::empty(),
                _memory: Arc::clone(memory),
            })?;
            owner.native = std::ptr::read(std::ptr::from_ref(node).cast::<$native>());
            node.release = Some($release);
            node.private_data = Box::into_raw(owner).cast();
            Ok(())
        }
    };
}

wrap_node!(
    wrap_schema,
    UQAArrowSchema,
    FFI_ArrowSchema,
    SchemaOwner,
    release_schema
);
wrap_node!(
    wrap_array,
    UQAArrowArray,
    FFI_ArrowArray,
    ArrayOwner,
    release_array
);
