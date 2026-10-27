//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range constructors copy inputs and admit the returned owner before invoking native SQL input.

use super::{boundary, UQAError, UQAValue, ValueSource};
use crate::{
    abi::{
        UQAStatus, UQA_OK, UQA_RANGE_EMPTY, UQA_RANGE_LOWER_INCLUSIVE, UQA_RANGE_LOWER_UNBOUNDED,
        UQA_RANGE_UPPER_INCLUSIVE, UQA_RANGE_UPPER_UNBOUNDED,
    },
    session::UQAEngine,
};
use std::{ffi::c_char, sync::Arc};
use uqa_core::Value;
use uqa_sql::{params::RangeParameterBounds, SQLParam};

// Spans and borrowed handles follow the public C lifetime contract.
#[allow(unsafe_code)]
unsafe fn range(
    engine: *mut UQAEngine,
    type_name: &str,
    lower: *const UQAValue,
    upper: *const UQAValue,
    flags: u32,
    out: &mut *mut UQAValue,
) -> boundary::Result<UQAStatus> {
    let empty = flags & UQA_RANGE_EMPTY != 0;
    if flags & !31 != 0
        || (empty && (flags != UQA_RANGE_EMPTY || !lower.is_null() || !upper.is_null()))
        || (flags & UQA_RANGE_LOWER_UNBOUNDED != 0 && !lower.is_null())
        || (flags & UQA_RANGE_UPPER_UNBOUNDED != 0 && !upper.is_null())
    {
        return Err(UQAError::invalid("inconsistent range bounds or flags"));
    }
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let control = session.control();
    let lower = if lower.is_null() {
        None
    } else {
        Some(crate::execute::parameter(session, boundary::input(lower)?)?)
    };
    let upper = if upper.is_null() {
        None
    } else {
        Some(crate::execute::parameter(session, boundary::input(upper)?)?)
    };
    let placeholder = control.finish(SQLParam::Scalar(Value::Null), control.empty_reservation())?;
    let mut value = UQAValue::owned(placeholder, Some(Arc::clone(session)))?;
    let bounds = if empty {
        RangeParameterBounds::Empty
    } else {
        RangeParameterBounds::Bounds {
            lower: lower.as_deref(),
            upper: upper.as_deref(),
            lower_inclusive: flags & UQA_RANGE_LOWER_INCLUSIVE != 0,
            upper_inclusive: flags & UQA_RANGE_UPPER_INCLUSIVE != 0,
        }
    };
    let parameter = session.execute(|native| {
        native.create_range_parameter_with_control(type_name, bounds, &control)
    })?;
    value.source = ValueSource::Owned(parameter);
    *out = Box::into_raw(value);
    Ok(UQA_OK)
}

#[allow(unsafe_code)]
unsafe fn multirange(
    engine: *mut UQAEngine,
    type_name: &str,
    ranges: *const *const UQAValue,
    count: u32,
    out: &mut *mut UQAValue,
) -> boundary::Result<UQAStatus> {
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let control = session.control();
    let members = crate::execute::parameters(session, ranges, count)?;
    let placeholder = control.finish(SQLParam::Scalar(Value::Null), control.empty_reservation())?;
    let mut value = UQAValue::owned(placeholder, Some(Arc::clone(session)))?;
    let parameter = session.execute(|native| {
        native.create_multirange_parameter_with_control(type_name, &members, &control)
    })?;
    value.source = ValueSource::Owned(parameter);
    *out = Box::into_raw(value);
    Ok(UQA_OK)
}

ffi_fn!(uqa_value_create_range(engine: *mut UQAEngine, type_name: *const c_char, lower: *const UQAValue, upper: *const UQAValue, flags: u32, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    range(engine, boundary::text(type_name)?, lower, upper, flags, out)
});
ffi_fn!(uqa_value_create_range_n(engine: *mut UQAEngine, type_name: *const c_char, type_name_size: usize, lower: *const UQAValue, upper: *const UQAValue, flags: u32, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    if type_name.is_null() { return Err(UQAError::invalid("type name must not be NULL")); }
    range(engine, boundary::text_n(type_name, type_name_size)?, lower, upper, flags, out)
});
ffi_fn!(uqa_value_create_multirange(engine: *mut UQAEngine, type_name: *const c_char, ranges: *const *const UQAValue, count: u32, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    multirange(engine, boundary::text(type_name)?, ranges, count, out)
});
ffi_fn!(uqa_value_create_multirange_n(engine: *mut UQAEngine, type_name: *const c_char, type_name_size: usize, ranges: *const *const UQAValue, count: u32, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    if type_name.is_null() { return Err(UQAError::invalid("type name must not be NULL")); }
    multirange(engine, boundary::text_n(type_name, type_name_size)?, ranges, count, out)
});
