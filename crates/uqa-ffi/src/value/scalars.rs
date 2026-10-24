//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact scalar input and access reuse SQL's admitted value representations.

use super::{boundary, wrong_type, ScalarAccess, UQAError, UQAValue};
use crate::abi::{UQAStringView, UQA_DONE, UQA_ERROR_RANGE, UQA_OK};
use std::ffi::c_char;
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::{ast::ColumnType, SQLParam};

fn parse_scalar(text: &str, ty: ColumnType) -> boundary::Result<*mut UQAValue> {
    let control = ProductionControl::uncontrolled();
    let text = control.copy_text(text)?;
    let (text, memory) = text.into_parts();
    let input = control.finish(Value::Str(text), memory)?;
    let name = ty.sql_name_with_control(&control)?;
    let value = uqa_sql::expr::cast_value_from_with_control(&input, &name, None, &control)?;
    let (value, memory) = value.into_parts();
    let parameter = control.finish(SQLParam::typed_scalar(value, ty), memory)?;
    Ok(Box::into_raw(UQAValue::owned(parameter, None)?))
}

macro_rules! text_constructor {
    ($name:ident, $sized:ident, $ty:expr) => {
        ffi_fn!($name(value: *const c_char, out: *mut *mut UQAValue) {
            let out = boundary::new_handle(out)?;
            *out = parse_scalar(boundary::text(value)?, $ty)?;
            Ok(UQA_OK)
        });
        ffi_fn!($sized(value: *const c_char, size: usize, out: *mut *mut UQAValue) {
            let out = boundary::new_handle(out)?;
            *out = parse_scalar(boundary::text_n(value, size)?, $ty)?;
            Ok(UQA_OK)
        });
    };
}

text_constructor!(
    uqa_value_create_numeric,
    uqa_value_create_numeric_n,
    ColumnType::Numeric {
        precision: None,
        scale: None
    }
);
text_constructor!(
    uqa_value_create_json,
    uqa_value_create_json_n,
    ColumnType::Json
);
text_constructor!(
    uqa_value_create_jsonb,
    uqa_value_create_jsonb_n,
    ColumnType::JsonB
);

macro_rules! integer_getter {
    ($name:ident, $ty:ty, $access:ident) => {
        ffi_fn!($name(value: *const UQAValue, out: *mut $ty) {
            let out = boundary::output(out)?;
            let value = boundary::input(value)?;
            let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
            value.check_access(ScalarAccess::$access)?;
            let observed = value.observed()?;
            let Value::Int(number) = observed else { return Err(wrong_type(observed)); };
            *out = <$ty>::try_from(*number).map_err(|_| UQAError::new(
                UQA_ERROR_RANGE, "integer carrier is outside the declared width",
            ))?;
            Ok(UQA_OK)
        });
    };
}

integer_getter!(uqa_value_get_int16, i16, Int16);
integer_getter!(uqa_value_get_int32, i32, Int32);

// The SQL REAL descriptor permits only f32 values; the round trip checks native carrier integrity.
#[allow(clippy::cast_possible_truncation)]
fn real(value: f64) -> boundary::Result<f32> {
    let narrowed = value as f32;
    if value.is_nan() || f64::from(narrowed).to_bits() == value.to_bits() {
        Ok(narrowed)
    } else {
        Err(UQAError::new(
            UQA_ERROR_RANGE,
            "real carrier is outside the declared width",
        ))
    }
}

ffi_fn!(uqa_value_get_float32(value: *const UQAValue, out: *mut f32) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    value.check_access(ScalarAccess::Float32)?;
    match value.observed()? {
        Value::Float(number) => { *out = real(*number)?; Ok(UQA_OK) },
        value => Err(wrong_type(value)),
    }
});

ffi_fn!(uqa_value_get_numeric(value: *const UQAValue, out: *mut UQAStringView) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    value.check_access(ScalarAccess::Numeric)?;
    let observed = value.observed()?;
    let Value::Decimal(number) = observed else { return Err(wrong_type(observed)); };
    if value.numeric_text.get().is_none() {
        let text = number.to_sql_string_with_control(&value.control())?;
        value.numeric_text.set(text).map_err(|_| UQAError::state("numeric value was already formatted"))?;
    }
    *out = value.numeric_text.get().ok_or_else(|| UQAError::state("numeric formatter returned no text"))?.as_str().into();
    Ok(UQA_OK)
});

ffi_fn!(uqa_value_get_numeric_scale(value: *const UQAValue, out: *mut u32) {
    let out = boundary::output(out)?;
    let value = boundary::input(value)?;
    let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
    value.check_access(ScalarAccess::Numeric)?;
    let observed = value.observed()?;
    let Value::Decimal(number) = observed else { return Err(wrong_type(observed)); };
    match number.display_scale() {
        Some(scale) => { *out = scale; Ok(UQA_OK) },
        None => Ok(UQA_DONE),
    }
});
