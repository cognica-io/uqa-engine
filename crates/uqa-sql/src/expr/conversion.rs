//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar coercion, checked numeric conversion, and vector/tensor decoding.

use super::{out_of_range, ArrayValue, DecimalValue, Result, SQLError, Value};

use uqa_core::memory::{Produced, ProductionControl, ProductionString, ProductionVec};

pub fn value_to_string(value: &Value) -> String {
    value_to_string_with_control(value, &ProductionControl::uncontrolled())
        .expect("ordinary value text production")
        .into_uncontrolled()
        .expect("ordinary value text")
}

pub fn value_to_string_with_control(
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    control.check()?;
    Ok(match value {
        Value::Null | Value::Void => control.copy_text("")?,
        Value::Int(value) => control.format(format_args!("{value}"))?,
        Value::Float(value) => uqa_core::format_float_pg_with_control(*value, control)?,
        Value::Decimal(value) => value.to_sql_string_with_control(control)?,
        Value::Str(value) | Value::Json(value) | Value::JsonB(value) => control.copy_text(value)?,
        Value::FixedChar(value) => control.copy_text(value.trim_end_matches(' '))?,
        Value::Bool(value) => control.copy_text(if *value { "true" } else { "false" })?,
        Value::Temporal(value) => value.to_sql_string_with_control(control)?,
        Value::Array(value) => array_value_to_string_with_control(value, control)?,
        Value::List(_) | Value::Map(_) => {
            return super::json::format_value_as_json_with_control(value, control)
        }
        Value::Row(values) => composite_value_to_string(values.iter(), control)?,
        Value::Record(fields) => {
            composite_value_to_string(fields.iter().map(|(_, value)| value), control)?
        }
        Value::Bytes(values) => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut text = ProductionString::new(*control);
            text.push_str("\\x")?;
            for byte in values {
                text.push(char::from(HEX[usize::from(byte >> 4)]))?;
                text.push(char::from(HEX[usize::from(byte & 0xf)]))?;
            }
            text.finish()?
        }
    })
}

/// `PostgreSQL`'s legacy vector text format separates values with spaces.
pub fn vector_value_to_string(value: &Value) -> Option<String> {
    vector_value_to_string_with_control(value, &ProductionControl::uncontrolled())
        .expect("ordinary vector formatting")
        .map(|text| text.into_uncontrolled().expect("ordinary vector text"))
}

pub(super) fn vector_value_to_string_with_control(
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<String>>> {
    control.check()?;
    let elements = match value {
        Value::List(elements) => elements.as_slice(),
        Value::Array(array) if array.dimensions().len() <= 1 => array.elements(),
        _ => return Ok(None),
    };
    let mut text = ProductionString::new(*control);
    for (index, value) in elements.iter().enumerate() {
        if index != 0 {
            text.push(' ')?;
        }
        text.push_str(&value_to_string_with_control(value, control)?)?;
    }
    Ok(Some(text.finish()?))
}

pub fn array_value_to_string(array: &ArrayValue) -> String {
    array_value_to_string_with_control(array, &ProductionControl::uncontrolled())
        .expect("ordinary array formatting")
        .into_uncontrolled()
        .expect("ordinary array text")
}

pub(super) fn array_value_to_string_with_control(
    array: &ArrayValue,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut text = ProductionString::new(*control);
    append_array(&mut text, array, control)?;
    Ok(text.finish()?)
}

fn append_array(
    text: &mut ProductionString<'_>,
    array: &ArrayValue,
    control: &ProductionControl<'_>,
) -> Result<()> {
    if array.lower_bounds().iter().any(|lower| *lower != 1) {
        for (lower, length) in array.lower_bounds().iter().zip(array.dimensions()) {
            let upper = i64::from(*lower) + i64::try_from(*length).unwrap_or(i64::MAX) - 1;
            text.push_str(&control.format(format_args!("[{lower}:{upper}]"))?)?;
        }
        text.push('=')?;
    }
    append_array_elements(text, array.elements(), control)
}

fn append_array_elements(
    text: &mut ProductionString<'_>,
    elements: &[Value],
    control: &ProductionControl<'_>,
) -> Result<()> {
    text.push('{')?;
    for (index, value) in elements.iter().enumerate() {
        if index != 0 {
            text.push(',')?;
        }
        match value {
            Value::Null => text.push_str("NULL")?,
            Value::Bool(value) => text.push_str(if *value { "t" } else { "f" })?,
            Value::List(values) => append_array_elements(text, values, control)?,
            Value::Array(array) => append_array(text, array, control)?,
            other => {
                let value = value_to_string_with_control(other, control)?;
                let mut quoted = value.is_empty() || value.eq_ignore_ascii_case("null");
                for character in value.chars() {
                    control.check()?;
                    quoted |= character.is_whitespace()
                        || matches!(character, ',' | '{' | '}' | '"' | '\\');
                }
                append_escaped(text, &value, quoted, false)?;
            }
        }
    }
    text.push('}')?;
    Ok(())
}

fn composite_value_to_string<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut text = ProductionString::new(*control);
    text.push('(')?;
    for (index, value) in values.into_iter().enumerate() {
        if index != 0 {
            text.push(',')?;
        }
        if matches!(value, Value::Null) {
            continue;
        }
        let value = match value {
            Value::Bool(value) => control.copy_text(if *value { "t" } else { "f" })?,
            other => value_to_string_with_control(other, control)?,
        };
        let mut quoted = value.is_empty();
        for byte in value.bytes() {
            control.check()?;
            quoted |=
                matches!(byte, b',' | b'(' | b')' | b'"' | b'\\') || byte.is_ascii_whitespace();
        }
        append_escaped(&mut text, &value, quoted, true)?;
    }
    text.push(')')?;
    Ok(text.finish()?)
}

fn append_escaped(
    text: &mut ProductionString<'_>,
    value: &str,
    quoted: bool,
    composite: bool,
) -> Result<()> {
    if !quoted {
        text.push_str(value)?;
        return Ok(());
    }
    text.push('"')?;
    for character in value.chars() {
        if character == '\\' || (character == '"' && !composite) {
            text.push('\\')?;
        }
        if character == '"' && composite {
            text.push('"')?;
        }
        text.push(character)?;
    }
    text.push('"')?;
    Ok(())
}

pub(super) fn expect_str(args: &[Value], idx: usize) -> Result<String> {
    args.get(idx)
        .map(value_to_string)
        .ok_or_else(|| SQLError::TypeMismatch(format!("missing arg #{idx}")))
}

pub(super) fn string1<F: FnOnce(&str) -> String>(args: &[Value], f: F) -> Result<Value> {
    if args.is_empty() {
        return Err(SQLError::TypeMismatch("string fn needs 1 arg".into()));
    }
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    let s = value_to_string(&args[0]);
    Ok(Value::Str(f(&s)))
}

pub(super) fn float1_with_control<F: FnOnce(f64) -> f64>(
    args: &[Value],
    name: &str,
    f: F,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    control.check()?;
    if args.len() != 1 {
        return Err(SQLError::TypeMismatch(format!("{name} takes 1 arg")));
    }
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    Ok(Value::Float(f(to_f64_with_control(&args[0], control)?)))
}

pub(super) fn initcap_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for ch in s.chars() {
        if ch.is_whitespace() {
            out.push(ch);
            start = true;
            continue;
        }
        if start {
            for c in ch.to_uppercase() {
                out.push(c);
            }
            start = false;
        } else {
            for c in ch.to_lowercase() {
                out.push(c);
            }
        }
    }
    out
}

pub(super) fn to_i64(v: &Value) -> Result<i64> {
    to_i64_with_control(v, &ProductionControl::uncontrolled())
}

pub(super) fn to_i64_with_control(v: &Value, control: &ProductionControl<'_>) -> Result<i64> {
    control.check()?;
    match v {
        Value::Int(n) => Ok(*n),
        Value::Float(f) => float_to_i64_trunc(*f),
        Value::Decimal(d) => d
            .to_i64_trunc_with_control(control)?
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to integer"))),
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Str(s) | Value::FixedChar(s) => s
            .trim()
            .parse()
            .map_err(|_| SQLError::TypeMismatch(format!("cannot parse {s:?} as integer"))),
        other => Err(SQLError::TypeMismatch(format!(
            "expected integer, got {other:?}"
        ))),
    }
}

pub(super) fn nonnegative_usize(value: i64, label: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| SQLError::Routine {
        sqlstate: "22003".into(),
        message: format!("{label} exceeds the platform addressable range"),
    })
}

pub(super) fn allocation_error(label: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "53200".into(),
        message: format!("{label} result exceeds available memory"),
    }
}

pub(crate) fn to_f64(v: &Value) -> Result<f64> {
    to_f64_with_control(v, &ProductionControl::uncontrolled())
}

pub(crate) fn to_f64_with_control(v: &Value, control: &ProductionControl<'_>) -> Result<f64> {
    super::floating::to_float_with_control(v, super::FloatWidth::DoublePrecision, control)
}

pub(super) fn to_decimal(value: &Value) -> Result<DecimalValue> {
    to_decimal_with_control(value, &ProductionControl::uncontrolled())?
        .into_uncontrolled()
        .map_err(|_| SQLError::Internal("ordinary numeric production owner".into()))
}

pub(super) fn to_decimal_with_control(
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<DecimalValue>> {
    control.check()?;
    match value {
        Value::Decimal(value) => Ok(value.clone_with_control(control)?),
        Value::Int(value) => Ok(DecimalValue::from_i64_with_control(*value, control)?),
        Value::Bool(value) => Ok(DecimalValue::from_i64_with_control(
            i64::from(*value),
            control,
        )?),
        Value::Float(number) => DecimalValue::from_f64_lossy_with_control(*number, control)?
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {value:?} to numeric"))),
        Value::Str(text) | Value::FixedChar(text) => {
            DecimalValue::parse_with_control(text, control)?.ok_or_else(|| SQLError::Routine {
                sqlstate: "22P02".into(),
                message: format!("invalid input syntax for type numeric: \"{text}\""),
            })
        }
        other => Err(SQLError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

pub(super) fn float_to_i64_trunc(value: f64) -> Result<i64> {
    if !value.is_finite() || value < i64::MIN as f64 || value >= 9_223_372_036_854_775_808.0 {
        return Err(out_of_range("bigint"));
    }
    Ok(value.trunc() as i64)
}

pub(super) fn float_to_i64_rounded(value: f64, type_name: &str) -> Result<i64> {
    let rounded = value.round();
    if !rounded.is_finite() || rounded < i64::MIN as f64 || rounded >= 9_223_372_036_854_775_808.0 {
        return Err(out_of_range(type_name));
    }
    Ok(rounded as i64)
}

pub(super) fn gcd_i64(a: i64, b: i64) -> Result<i64> {
    let mut a = a.unsigned_abs();
    let mut b = b.unsigned_abs();
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    i64::try_from(a).map_err(|_| out_of_range("bigint"))
}

/// Best-effort `Value -> i64`. Returns `None` for shapes that do not
/// have a well-defined integer projection (e.g. `Value::Null`).
pub(super) fn coerce_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        Value::Float(f) => float_to_i64_trunc(*f).ok(),
        Value::Decimal(d) => d.to_i64_trunc(),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Str(s) | Value::FixedChar(s) => s.parse().ok(),
        _ => None,
    }
}

/// Coerce a [`Value`] into a `Vec<f32>` if it is a homogeneous numeric
/// list (used to read vector literals from `ARRAY[...]` or `$N` Vector
/// params).
pub fn value_to_vector(v: &Value) -> Result<Vec<f32>> {
    value_to_vector_with_control(v, &ProductionControl::uncontrolled())?
        .into_uncontrolled()
        .map_err(|_| SQLError::Internal("ordinary vector owner".into()))
}

pub fn value_to_vector_with_control(
    v: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<f32>>> {
    let items = vector_items(v)?;
    let mut out = ProductionVec::new(*control);
    out.reserve(items.len())?;
    for item in items {
        out.push_copy(vector_element_with_control(item, control)?)?;
    }
    Ok(out.finish()?)
}

pub(crate) fn vector_items(v: &Value) -> Result<&[Value]> {
    match v {
        Value::List(items) => Ok(items.as_slice()),
        Value::Array(array) if array.dimensions().len() <= 1 => Ok(array.elements()),
        Value::Array(array) => Err(SQLError::TypeMismatch(format!(
            "expected one-dimensional vector input, got {} dimensions",
            array.dimensions().len()
        ))),
        other => Err(SQLError::TypeMismatch(format!(
            "expected vector (numeric array), got {other:?}"
        ))),
    }
}

pub(crate) fn vector_element(item: &Value) -> Result<f32> {
    vector_element_with_control(item, &ProductionControl::uncontrolled())
}

pub(crate) fn vector_element_with_control(
    item: &Value,
    control: &ProductionControl<'_>,
) -> Result<f32> {
    control.check()?;
    match item {
        Value::Float(f) => numeric_f64_to_f32(*f, item),
        Value::Int(i) => Ok(*i as f32),
        Value::Decimal(d) => numeric_f64_to_f32(
            d.to_f64_with_control(control)?.ok_or_else(|| {
                SQLError::TypeMismatch(format!("vector element must fit f32, got {item:?}"))
            })?,
            item,
        ),
        other => Err(SQLError::TypeMismatch(format!(
            "vector element must be numeric, got {other:?}"
        ))),
    }
}

pub(super) fn numeric_f64_to_f32(value: f64, source: &Value) -> Result<f32> {
    if !value.is_finite() || value < -(f32::MAX as f64) || value > f32::MAX as f64 {
        return Err(SQLError::TypeMismatch(format!(
            "vector element must be finite and fit f32, got {source:?}"
        )));
    }
    Ok(value as f32)
}

/// Coerce a [`Value`] into a tensor: an array of homogeneous numeric
/// vectors. Used by `TENSOR(N)` columns to store chunk embeddings for one
/// row while still indexing each vector element.
pub fn value_to_tensor(v: &Value) -> Result<Vec<Vec<f32>>> {
    value_to_tensor_with_control(v, &ProductionControl::uncontrolled())?
        .into_uncontrolled()
        .map_err(|_| SQLError::Internal("ordinary tensor owner".into()))
}

pub fn value_to_tensor_with_control(
    v: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Vec<f32>>>> {
    let items = tensor_items(v)?;
    let mut out = ProductionVec::new(*control);
    out.reserve(items.len())?;
    for item in items {
        out.push_produced(value_to_vector_with_control(item, control)?)?;
    }
    Ok(out.finish()?)
}

pub(crate) fn tensor_items(v: &Value) -> Result<&[Value]> {
    match v {
        Value::List(items) => Ok(items.as_slice()),
        Value::Array(array) if array.dimensions().is_empty() || array.dimensions().len() == 2 => {
            Ok(array.elements())
        }
        Value::Array(array) => Err(SQLError::TypeMismatch(format!(
            "expected two-dimensional tensor input, got {} dimensions",
            array.dimensions().len()
        ))),
        other => Err(SQLError::TypeMismatch(format!(
            "expected tensor (array of numeric arrays), got {other:?}"
        ))),
    }
}
