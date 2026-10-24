//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact physical column construction after descriptor and capacity admission.

use super::{arrow_error, overflow, unsupported, ArrowExportPolicy, ArrowRows};
use arrow_array::{
    builder::{LargeBinaryBuilder, LargeStringBuilder},
    ArrayRef, BooleanArray, Decimal128Array, FixedSizeBinaryArray, Float32Array, Float64Array,
    Int16Array, Int32Array, Int64Array,
};
use arrow_schema::DataType;
use std::sync::Arc;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::{expr::EngineHook, ColumnType, SQLError};

fn mismatch() -> SQLError {
    SQLError::TypeMismatch("native value disagrees with its Arrow export descriptor".into())
}

fn observe<R>(
    source: &dyn ArrowRows,
    column: usize,
    row: usize,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
    read: impl FnOnce(&Value) -> Result<R, SQLError>,
) -> Result<R, SQLError> {
    control.check()?;
    let value = source.value_at(row, column).ok_or_else(mismatch)?;
    if let Value::Datum(datum) = value {
        let decoded = uqa_sql::expr::datums::read_with_catalog_and_control(datum, engine, control)?;
        read(&decoded)
    } else {
        read(value)
    }
}

fn primitives<T: Copy>(
    source: &dyn ArrowRows,
    column: usize,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
    read: impl Fn(&Value) -> Result<T, SQLError>,
) -> Result<Produced<Vec<Option<T>>>, SQLError> {
    let mut values = ProductionVec::new(*control);
    values.reserve(source.row_count())?;
    for row in 0..source.row_count() {
        values.push_copy(observe(source, column, row, engine, control, |value| {
            if matches!(value, Value::Null) {
                Ok(None)
            } else {
                read(value).map(Some)
            }
        })?)?;
    }
    Ok(values.finish()?)
}

fn integer(value: &Value) -> Result<i64, SQLError> {
    match value {
        Value::Int(value) => Ok(*value),
        _ => Err(mismatch()),
    }
}

fn float(value: &Value) -> Result<f64, SQLError> {
    match value {
        Value::Float(value) => Ok(*value),
        _ => Err(mismatch()),
    }
}

pub(super) fn build(
    source: &dyn ArrowRows,
    column: usize,
    output: &DataType,
    policy: ArrowExportPolicy,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ArrayRef>, SQLError> {
    macro_rules! primitive {
        ($array:ident, $read:expr) => {{
            let values = primitives(source, column, engine, control, $read)?;
            let (values, scratch) = values.into_parts();
            let array = Arc::new($array::from(values));
            drop(scratch);
            array as ArrayRef
        }};
    }
    let array: ArrayRef = match output {
        DataType::Boolean => primitive!(BooleanArray, |value| match value {
            Value::Bool(value) => Ok(*value),
            _ => Err(mismatch()),
        }),
        DataType::Int16 => primitive!(Int16Array, |value| {
            i16::try_from(integer(value)?).map_err(|_| mismatch())
        }),
        DataType::Int32 => primitive!(Int32Array, |value| {
            i32::try_from(integer(value)?).map_err(|_| mismatch())
        }),
        DataType::Int64 => primitive!(Int64Array, integer),
        DataType::Float32 => primitive!(Float32Array, |value| {
            let value = float(value)?;
            let narrow = value as f32;
            if value.is_nan() || f64::from(narrow) == value {
                Ok(narrow)
            } else {
                Err(mismatch())
            }
        }),
        DataType::Float64 => primitive!(Float64Array, float),
        DataType::Decimal128(precision, scale) => {
            let values = primitives(source, column, engine, control, |value| {
                let Value::Decimal(value) = value else {
                    return Err(mismatch());
                };
                let text = value.to_sql_string_with_control(control)?;
                decimal_coefficient(&text, *precision, *scale)
            })?;
            let (values, scratch) = values.into_parts();
            let array = Decimal128Array::from(values)
                .with_precision_and_scale(*precision, *scale)
                .map_err(|error| arrow_error(&error))?;
            drop(scratch);
            Arc::new(array)
        }
        DataType::FixedSizeBinary(16) => {
            let values = primitives(source, column, engine, control, |value| {
                let Value::Str(text) = value else {
                    return Err(mismatch());
                };
                uqa_sql::expr::parse_uuid_bytes_with_control(text, control)
            })?;
            Arc::new(
                FixedSizeBinaryArray::try_from_sparse_iter_with_size(
                    values
                        .iter()
                        .map(|value| value.as_ref().map(<[u8; 16]>::as_slice)),
                    16,
                )
                .map_err(|error| arrow_error(&error))?,
            )
        }
        DataType::LargeUtf8 => return text(source, column, policy, engine, control),
        DataType::LargeBinary => return binary(source, column, engine, control),
        _ => return Err(unsupported("Arrow physical type is not supported")),
    };
    Ok(control.finish(array, control.empty_reservation())?)
}

fn text(
    source: &dyn ArrowRows,
    column: usize,
    policy: ArrowExportPolicy,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ArrayRef>, SQLError> {
    let mut values = ProductionVec::new(*control);
    values.reserve(source.row_count())?;
    let mut bytes = 0usize;
    for row in 0..source.row_count() {
        let value = observe(source, column, row, engine, control, |value| {
            if matches!(value, Value::Null) {
                return Ok(control.finish(None, control.empty_reservation())?);
            }
            let text = if policy == ArrowExportPolicy::SQLText {
                let ty = source
                    .column_type(column)
                    .ok_or_else(|| unsupported("SQL text export requires a native descriptor"))?;
                uqa_sql::result::format_postgres_text_with_control(value, ty, engine, control)?
            } else {
                let ((Some(ColumnType::Json), Value::Json(text))
                | (Some(ColumnType::JsonB), Value::JsonB(text))
                | (Some(ColumnType::Text | ColumnType::Varchar(_)), Value::Str(text))
                | (Some(ColumnType::Bpchar | ColumnType::Character(_)), Value::FixedChar(text))) =
                    (source.column_type(column), value)
                else {
                    return Err(mismatch());
                };
                control.copy_text(text)?
            };
            bytes = bytes.checked_add(text.len()).ok_or_else(overflow)?;
            let (text, memory) = text.into_parts();
            Ok(control.finish(Some(text), memory)?)
        })?;
        values.push_produced(value)?;
    }
    let values = values.finish()?;
    let memory = control.reserve(bytes.checked_add(64).ok_or_else(overflow)?)?;
    let mut array = LargeStringBuilder::with_capacity(values.len(), bytes);
    for value in values.iter() {
        control.check()?;
        array.append_option(value.as_deref());
    }
    let array: ArrayRef = Arc::new(array.finish());
    Ok(control.finish(array, memory)?)
}

fn binary(
    source: &dyn ArrowRows,
    column: usize,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ArrayRef>, SQLError> {
    let mut values = ProductionVec::new(*control);
    values.reserve(source.row_count())?;
    let mut bytes = 0usize;
    for row in 0..source.row_count() {
        let value = observe(source, column, row, engine, control, |value| {
            if matches!(value, Value::Null) {
                return Ok(control.finish(None, control.empty_reservation())?);
            }
            let Value::Bytes(value) = value else {
                return Err(mismatch());
            };
            bytes = bytes.checked_add(value.len()).ok_or_else(overflow)?;
            let mut copied = ProductionVec::new(*control);
            copied.extend_from_slice(value)?;
            let (value, memory) = copied.finish()?.into_parts();
            Ok(control.finish(Some(value), memory)?)
        })?;
        values.push_produced(value)?;
    }
    let values = values.finish()?;
    let memory = control.reserve(bytes.checked_add(64).ok_or_else(overflow)?)?;
    let mut array = LargeBinaryBuilder::with_capacity(values.len(), bytes);
    for value in values.iter() {
        control.check()?;
        array.append_option(value.as_deref());
    }
    let array: ArrayRef = Arc::new(array.finish());
    Ok(control.finish(array, memory)?)
}

/// Decode the exact decimal output representation into Arrow's scaled integer.
/// Discarded digits must all be zero: this conversion never rounds SQL values.
fn decimal_coefficient(text: &str, precision: u8, scale: i8) -> Result<i128, SQLError> {
    let invalid = || unsupported("NUMERIC value has no lossless Arrow decimal representation");
    let (negative, text) = text
        .strip_prefix('-')
        .map_or((false, text), |text| (true, text));
    if text.is_empty()
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return Err(invalid());
    }
    let fractional = text.split_once('.').map_or(0, |(_, tail)| tail.len());
    let adjustment = i64::from(scale) - i64::try_from(fractional).map_err(|_| invalid())?;
    let digits = text.bytes().filter(|byte| *byte != b'.');
    let length = text.len() - usize::from(text.contains('.'));
    let discard = if adjustment < 0 {
        usize::try_from(-adjustment).map_err(|_| invalid())?
    } else {
        0
    };
    if discard > length
        && text
            .bytes()
            .any(|byte| byte.is_ascii_digit() && byte != b'0')
    {
        return Err(invalid());
    }
    let retained = length.saturating_sub(discard);
    let mut coefficient = 0i128;
    for (index, digit) in digits.enumerate() {
        if index < retained {
            coefficient = coefficient
                .checked_mul(10)
                .and_then(|value| value.checked_add(i128::from(digit - b'0')))
                .ok_or_else(invalid)?;
        } else if digit != b'0' {
            return Err(invalid());
        }
    }
    if adjustment > 0 {
        coefficient = coefficient
            .checked_mul(
                10_i128
                    .checked_pow(u32::try_from(adjustment).map_err(|_| invalid())?)
                    .ok_or_else(invalid)?,
            )
            .ok_or_else(invalid)?;
    }
    if coefficient
        >= 10_i128
            .checked_pow(u32::from(precision))
            .ok_or_else(invalid)?
    {
        return Err(invalid());
    }
    Ok(if negative { -coefficient } else { coefficient })
}
