//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Copy C spans and reserve the returned owner before native array input can execute.

use super::{boundary, UQAError, UQAValue, ValueSource};
use crate::{
    abi::{UQAArrayDimension, UQAStatus, UQA_OK},
    session::UQAEngine,
};
use std::{ffi::c_char, sync::Arc};
use uqa_core::{memory::ProductionVec, Value};
use uqa_sql::SQLParam;

// Span validity and exclusive output ownership follow the public C contract.
#[allow(unsafe_code)]
unsafe fn create(
    engine: *mut UQAEngine,
    type_name: &str,
    dimensions: *const UQAArrayDimension,
    rank: u32,
    elements: *const *const UQAValue,
    count: u64,
    out: &mut *mut UQAValue,
) -> boundary::Result<UQAStatus> {
    let dimensions = boundary::span(dimensions, rank as usize)?;
    let count = u32::try_from(count)
        .map_err(|_| UQAError::invalid("array element count exceeds the native range"))?;
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let control = session.control();
    let mut lengths = ProductionVec::new(control);
    let mut bounds = ProductionVec::new(control);
    for dimension in dimensions {
        lengths
            .push_copy(usize::try_from(dimension.length).map_err(|_| {
                UQAError::invalid("array dimension exceeds the addressable range")
            })?)?;
        bounds.push_copy(dimension.lower_bound)?;
    }
    let elements = crate::execute::parameters(session, elements, count)?;
    let empty = control.finish(SQLParam::Scalar(Value::Null), control.empty_reservation())?;
    let mut value = UQAValue::owned(empty, Some(Arc::clone(session)))?;
    let parameter = session.execute(|native| {
        native
            .create_array_parameter_with_control(type_name, &lengths, &bounds, &elements, &control)
    })?;
    value.source = ValueSource::Owned(parameter);
    *out = Box::into_raw(value);
    Ok(UQA_OK)
}

ffi_fn!(uqa_value_create_array(engine: *mut UQAEngine, type_name: *const c_char, dimensions: *const UQAArrayDimension, rank: u32, elements: *const *const UQAValue, count: u64, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    create(engine, boundary::text(type_name)?, dimensions, rank, elements, count, out)
});
ffi_fn!(uqa_value_create_array_n(engine: *mut UQAEngine, type_name: *const c_char, type_name_size: usize, dimensions: *const UQAArrayDimension, rank: u32, elements: *const *const UQAValue, count: u64, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    if type_name.is_null() { return Err(UQAError::invalid("type name must not be NULL")); }
    create(engine, boundary::text_n(type_name, type_name_size)?, dimensions, rank, elements, count, out)
});
