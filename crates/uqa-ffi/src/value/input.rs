//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-aware value creation reserves its public owner before SQL input can execute.

use super::{boundary, UQAError, UQAValue, ValueSource};
use crate::{abi::UQA_OK, session::UQAEngine};
use std::{ffi::c_char, sync::Arc};
use uqa_core::Value;
use uqa_sql::SQLParam;

// Address validity and exclusive output ownership come from the public C contract.
#[allow(unsafe_code)]
unsafe fn create(
    engine: *mut UQAEngine,
    type_name: &str,
    text: Option<&str>,
    out: &mut *mut UQAValue,
) -> boundary::Result<crate::abi::UQAStatus> {
    let session = &boundary::input(engine)?.session;
    let _call = session.enter()?;
    let control = session.control();
    let empty = control.finish(SQLParam::Scalar(Value::Null), control.empty_reservation())?;
    let mut value = UQAValue::owned(empty, Some(Arc::clone(session)))?;
    let parameter = session
        .native
        .create_typed_parameter_with_control(type_name, text, &control)
        .map_err(|error| session.execution_error(error))?;
    value.source = ValueSource::Owned(parameter);
    *out = Box::into_raw(value);
    Ok(UQA_OK)
}

ffi_fn!(uqa_value_create_typed_null(engine: *mut UQAEngine, type_name: *const c_char, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    create(engine, boundary::text(type_name)?, None, out)
});
ffi_fn!(uqa_value_create_typed_null_n(engine: *mut UQAEngine, type_name: *const c_char, type_name_size: usize, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    if type_name.is_null() { return Err(UQAError::invalid("type name must not be NULL")); }
    create(engine, boundary::text_n(type_name, type_name_size)?, None, out)
});
ffi_fn!(uqa_value_create_from_text(engine: *mut UQAEngine, type_name: *const c_char, text: *const c_char, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    create(engine, boundary::text(type_name)?, Some(boundary::text(text)?), out)
});
ffi_fn!(uqa_value_create_from_text_n(engine: *mut UQAEngine, type_name: *const c_char, type_name_size: usize, text: *const c_char, text_size: usize, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    if type_name.is_null() || text.is_null() { return Err(UQAError::invalid("type name and input text must not be NULL")); }
    create(engine, boundary::text_n(type_name, type_name_size)?, Some(boundary::text_n(text, text_size)?), out)
});
