//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admit C inputs and the return owner before native record construction.

use super::{boundary, UQAError, UQAValue, ValueSource};
use crate::{
    abi::{UQAStatus, UQA_OK},
    session::UQAEngine,
};
use std::{ffi::c_char, sync::Arc};
use uqa_core::Value;
use uqa_sql::SQLParam;

// The C caller keeps input spans readable for the complete synchronous call.
#[allow(unsafe_code)]
unsafe fn create(
    engine: *mut UQAEngine,
    type_name: &str,
    fields: *const *const UQAValue,
    count: u32,
    out: &mut *mut UQAValue,
) -> boundary::Result<UQAStatus> {
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let fields = crate::execute::parameters(session, fields, count)?;
    let control = session.control();
    let empty = control.finish(SQLParam::Scalar(Value::Null), control.empty_reservation())?;
    let mut value = UQAValue::owned(empty, Some(Arc::clone(session)))?;
    let parameter = session.execute(|native| {
        native.create_record_parameter_with_control(type_name, &fields, &control)
    })?;
    value.source = ValueSource::Owned(parameter);
    *out = Box::into_raw(value);
    Ok(UQA_OK)
}

ffi_fn!(uqa_value_create_record(engine: *mut UQAEngine, type_name: *const c_char, fields: *const *const UQAValue, count: u32, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    create(engine, boundary::text(type_name)?, fields, count, out)
});
ffi_fn!(uqa_value_create_record_n(engine: *mut UQAEngine, type_name: *const c_char, type_name_size: usize, fields: *const *const UQAValue, count: u32, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    if type_name.is_null() { return Err(UQAError::invalid("type name must not be NULL")); }
    create(engine, boundary::text_n(type_name, type_name_size)?, fields, count, out)
});
