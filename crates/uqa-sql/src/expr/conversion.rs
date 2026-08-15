//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar coercion, checked numeric conversion, and vector/tensor decoding.

use super::{
    hex_encode, out_of_range, value_to_json, ArrayValue, DecimalValue, Result, SQLError, Value,
};

pub fn value_to_string(v: &Value) -> String {
    match v {
        Value::Null => "".into(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Decimal(d) => d.to_sql_string(),
        Value::Str(s) => s.clone(),
        Value::FixedChar(s) => s.trim_end_matches(' ').to_string(),
        Value::Bool(b) => (if *b { "true" } else { "false" }).into(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::Json(text) | Value::JsonB(text) => text.clone(),
        Value::Array(array) => array_value_to_string(array),
        Value::List(_) | Value::Map(_) => value_to_json(v).to_string(),
        Value::Row(values) => composite_value_to_string(values.iter()),
        Value::Record(fields) => composite_value_to_string(fields.iter().map(|(_, value)| value)),
        // bytea renders as PostgreSQL hex output in text contexts.
        Value::Bytes(b) => format!("\\x{}", hex_encode(b)),
    }
}

pub fn array_value_to_string(array: &ArrayValue) -> String {
    let dimensions = if array
        .lower_bounds()
        .iter()
        .any(|lower_bound| *lower_bound != 1)
    {
        array
            .lower_bounds()
            .iter()
            .zip(array.dimensions())
            .map(|(lower, length)| {
                let upper = i64::from(*lower) + i64::try_from(*length).unwrap_or(i64::MAX) - 1;
                format!("[{lower}:{upper}]")
            })
            .collect::<String>()
            + "="
    } else {
        String::new()
    };
    format!("{dimensions}{}", array_elements_to_string(array.elements()))
}

fn array_elements_to_string(elements: &[Value]) -> String {
    let rendered = elements
        .iter()
        .map(|value| match value {
            Value::Null => "NULL".to_string(),
            Value::Bool(value) => if *value { "t" } else { "f" }.to_string(),
            Value::List(nested) => array_elements_to_string(nested),
            Value::Array(nested) => array_value_to_string(nested),
            other => {
                let text = value_to_string(other);
                let requires_quotes = text.is_empty()
                    || text.eq_ignore_ascii_case("null")
                    || text.chars().any(|character| {
                        character.is_whitespace()
                            || matches!(character, ',' | '{' | '}' | '"' | '\\')
                    });
                if requires_quotes {
                    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
                } else {
                    text
                }
            }
        })
        .collect::<Vec<_>>();
    format!("{{{}}}", rendered.join(","))
}

fn composite_value_to_string<'a>(values: impl IntoIterator<Item = &'a Value>) -> String {
    let fields = values
        .into_iter()
        .map(|value| {
            if matches!(value, Value::Null) {
                return String::new();
            }
            let text = value_to_string(value);
            if text.is_empty()
                || text.bytes().any(|byte| {
                    matches!(byte, b',' | b'(' | b')' | b'"' | b'\\') || byte.is_ascii_whitespace()
                })
            {
                format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\"\""))
            } else {
                text
            }
        })
        .collect::<Vec<_>>();
    format!("({})", fields.join(","))
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

pub(super) fn float1<F: FnOnce(f64) -> f64>(args: &[Value], name: &str, f: F) -> Result<Value> {
    if args.len() != 1 {
        return Err(SQLError::TypeMismatch(format!("{name} takes 1 arg")));
    }
    if matches!(args[0], Value::Null) {
        return Ok(Value::Null);
    }
    Ok(Value::Float(f(to_f64(&args[0])?)))
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
    match v {
        Value::Int(n) => Ok(*n),
        Value::Float(f) => float_to_i64_trunc(*f),
        Value::Decimal(d) => d
            .to_i64_trunc()
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
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        Value::Decimal(d) => d.to_f64().ok_or_else(|| {
            SQLError::TypeMismatch(format!("cannot cast {v:?} to double precision"))
        }),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        // float8 casts accept PostgreSQL's textual forms, including
        // Infinity / NaN spellings.
        Value::Str(s) | Value::FixedChar(s) => {
            let text = s.trim();
            let lowered = text.to_ascii_lowercase();
            match lowered.as_str() {
                "infinity" | "inf" | "+infinity" | "+inf" => Ok(f64::INFINITY),
                "-infinity" | "-inf" => Ok(f64::NEG_INFINITY),
                "nan" => Ok(f64::NAN),
                _ => text.parse().map_err(|_| SQLError::Routine {
                    sqlstate: "22P02".into(),
                    message: format!("invalid input syntax for type double precision: \"{s}\""),
                }),
            }
        }
        other => Err(SQLError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

pub(super) fn to_decimal(v: &Value) -> Result<DecimalValue> {
    match v {
        Value::Decimal(d) => Ok(d.clone()),
        Value::Int(n) => Ok(DecimalValue::from_i64(*n)),
        Value::Float(f) => DecimalValue::from_f64_lossy(*f)
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to numeric"))),
        Value::Bool(b) => Ok(DecimalValue::from_bool(*b)),
        Value::Str(s) | Value::FixedChar(s) => {
            DecimalValue::parse(s).ok_or_else(|| SQLError::Routine {
                sqlstate: "22P02".into(),
                message: format!("invalid input syntax for type numeric: \"{s}\""),
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
    let items = match v {
        Value::List(items) => items.as_slice(),
        Value::Array(array) if array.dimensions().len() <= 1 => array.elements(),
        Value::Array(array) => {
            return Err(SQLError::TypeMismatch(format!(
                "expected one-dimensional vector input, got {} dimensions",
                array.dimensions().len()
            )))
        }
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "expected vector (numeric array), got {other:?}"
            )))
        }
    };
    {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let x = match item {
                Value::Float(f) => numeric_f64_to_f32(*f, item)?,
                Value::Int(i) => *i as f32,
                Value::Decimal(d) => numeric_f64_to_f32(
                    d.to_f64().ok_or_else(|| {
                        SQLError::TypeMismatch(format!("vector element must fit f32, got {item:?}"))
                    })?,
                    item,
                )?,
                other => {
                    return Err(SQLError::TypeMismatch(format!(
                        "vector element must be numeric, got {other:?}"
                    )))
                }
            };
            out.push(x);
        }
        Ok(out)
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
    let items = match v {
        Value::List(items) => items.as_slice(),
        Value::Array(array) if array.dimensions().is_empty() || array.dimensions().len() == 2 => {
            array.elements()
        }
        Value::Array(array) => {
            return Err(SQLError::TypeMismatch(format!(
                "expected two-dimensional tensor input, got {} dimensions",
                array.dimensions().len()
            )))
        }
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "expected tensor (array of numeric arrays), got {other:?}"
            )))
        }
    };
    {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            out.push(value_to_vector(item)?);
        }
        Ok(out)
    }
}
