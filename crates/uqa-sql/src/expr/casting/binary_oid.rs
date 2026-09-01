//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OID-family and binary representation conversion.

use uqa_core::Value;

use crate::error::{Result, SQLError};

use super::{canonical_cast_source, out_of_range, split_type_modifier, undefined_cast};

pub(super) fn cast_oid(value: &Value, source_ty: Option<&str>) -> Result<Value> {
    let source = canonical_cast_source(source_ty, value);
    match (source.as_str(), value) {
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name",
            Value::Str(text) | Value::FixedChar(text),
        ) => parse_uint32_input(text, "oid"),
        ("int2", Value::Int(value)) => {
            let value = i16::try_from(*value).map_err(|_| out_of_range("smallint"))?;
            Ok(Value::Int(i64::from(i32::from(value) as u32)))
        }
        ("int4", Value::Int(value)) => {
            let value = i32::try_from(*value).map_err(|_| out_of_range("integer"))?;
            Ok(Value::Int(i64::from(value as u32)))
        }
        ("int8", Value::Int(value)) => u32::try_from(*value)
            .map(|value| Value::Int(i64::from(value)))
            .map_err(|_| SQLError::Routine {
                sqlstate: "22003".into(),
                message: "OID out of range".into(),
            }),
        (
            "oid" | "regclass" | "regcollation" | "regconfig" | "regdictionary" | "regnamespace"
            | "regoper" | "regoperator" | "regproc" | "regprocedure" | "regrole" | "regtype",
            Value::Int(value),
        ) => u32::try_from(*value)
            .map(|value| Value::Int(i64::from(value)))
            .map_err(|_| out_of_range("oid")),
        _ => Err(undefined_cast(&source, "oid")),
    }
}

pub(super) fn cast_regclass(value: &Value, source_ty: Option<&str>) -> Result<Value> {
    let source = canonical_cast_source(source_ty, value);
    match (source.as_str(), value) {
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name" | "regclass",
            Value::Str(text) | Value::FixedChar(text),
        ) => Ok(Value::Str(text.clone())),
        (_, Value::Int(_)) => cast_oid(value, source_ty),
        _ => Err(undefined_cast(&source, "regclass")),
    }
}

pub(super) fn cast_regnamespace(value: &Value, source_ty: Option<&str>) -> Result<Value> {
    let source = canonical_cast_source(source_ty, value);
    match (source.as_str(), value) {
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name" | "regnamespace",
            Value::Str(text) | Value::FixedChar(text),
        ) => Ok(Value::Str(text.clone())),
        (_, Value::Int(_)) => cast_oid(value, source_ty),
        _ => Err(undefined_cast(&source, "regnamespace")),
    }
}

pub(super) fn cast_regrole(value: &Value, source_ty: Option<&str>) -> Result<Value> {
    let source = canonical_cast_source(source_ty, value);
    match (source.as_str(), value) {
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name" | "regrole",
            Value::Str(text) | Value::FixedChar(text),
        ) => Ok(Value::Str(text.clone())),
        (_, Value::Int(_)) => cast_oid(value, source_ty),
        _ => Err(undefined_cast(&source, "regrole")),
    }
}

pub(super) fn cast_xid(value: &Value, source_ty: Option<&str>) -> Result<Value> {
    let source = canonical_cast_source(source_ty, value);
    match (source.as_str(), value) {
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name",
            Value::Str(text) | Value::FixedChar(text),
        ) => parse_uint32_input(text, "xid"),
        ("xid", Value::Int(value)) => u32::try_from(*value)
            .map(|value| Value::Int(i64::from(value)))
            .map_err(|_| out_of_range("xid")),
        _ => Err(undefined_cast(&source, "xid")),
    }
}

pub(super) fn cast_bytea(value: &Value, source_ty: Option<&str>) -> Result<Value> {
    let source = canonical_cast_source(source_ty, value);
    match (source.as_str(), value) {
        ("bytea", Value::Bytes(bytes)) => Ok(Value::Bytes(bytes.clone())),
        ("int2" | "int4" | "int8", Value::Int(value)) => integer_to_bytea(*value, Some(&source)),
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name",
            Value::Str(text) | Value::FixedChar(text),
        ) => parse_bytea_input(text),
        _ => Err(undefined_cast(&source, "bytea")),
    }
}

fn parse_bytea_input(text: &str) -> Result<Value> {
    if let Some(hex) = text.strip_prefix("\\x") {
        if !hex.len().is_multiple_of(2) {
            return Err(invalid_bytea(
                "invalid hexadecimal data: odd number of digits",
            ));
        }
        let mut bytes = Vec::with_capacity(hex.len() / 2);
        for pair in hex.as_bytes().chunks_exact(2) {
            let hi = (pair[0] as char)
                .to_digit(16)
                .ok_or_else(|| invalid_bytea("invalid hexadecimal digit"))?;
            let lo = (pair[1] as char)
                .to_digit(16)
                .ok_or_else(|| invalid_bytea("invalid hexadecimal digit"))?;
            bytes.push((hi * 16 + lo) as u8);
        }
        return Ok(Value::Bytes(bytes));
    }

    let input = text.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] != b'\\' {
            output.push(input[index]);
            index += 1;
            continue;
        }
        if input.get(index + 1) == Some(&b'\\') {
            output.push(b'\\');
            index += 2;
            continue;
        }
        let Some(octal) = input.get(index + 1..index + 4) else {
            return Err(invalid_bytea("invalid input syntax for type bytea"));
        };
        if !matches!(octal[0], b'0'..=b'3')
            || !octal[1..].iter().all(|byte| matches!(byte, b'0'..=b'7'))
        {
            return Err(invalid_bytea("invalid input syntax for type bytea"));
        }
        output.push((octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + (octal[2] - b'0'));
        index += 4;
    }
    Ok(Value::Bytes(output))
}

fn invalid_bytea(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: message.into(),
    }
}

fn parse_uint32_input(text: &str, target: &str) -> Result<Value> {
    let trimmed = text.trim();
    let digits = trimmed
        .strip_prefix('+')
        .or_else(|| trimmed.strip_prefix('-'))
        .unwrap_or(trimmed);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("invalid input syntax for type {target}: \"{text}\""),
        });
    }
    let parsed = trimmed.parse::<i128>().map_err(|_| SQLError::Routine {
        sqlstate: "22003".into(),
        message: format!("value \"{text}\" is out of range for type {target}"),
    })?;
    if !((i128::from(i32::MIN))..=i128::from(u32::MAX)).contains(&parsed) {
        return Err(SQLError::Routine {
            sqlstate: "22003".into(),
            message: format!("value \"{text}\" is out of range for type {target}"),
        });
    }
    let value = if parsed < 0 {
        u32::from_ne_bytes((parsed as i32).to_ne_bytes())
    } else {
        parsed as u32
    };
    Ok(Value::Int(i64::from(value)))
}

fn integer_to_bytea(value: i64, source_ty: Option<&str>) -> Result<Value> {
    let source = source_ty
        .map(split_type_modifier)
        .map(|(base, _)| base)
        .unwrap_or("integer");
    let bytes = match source {
        "smallint" | "int2" | "pg_catalog.int2" => i16::try_from(value)
            .map(i16::to_be_bytes)
            .map(|bytes| bytes.to_vec())
            .map_err(|_| out_of_range("smallint"))?,
        "bigint" | "int8" | "bigserial" | "serial8" | "pg_catalog.int8" => {
            value.to_be_bytes().to_vec()
        }
        "integer" | "int" | "int4" | "serial" | "serial4" | "pg_catalog.int4" => {
            i32::try_from(value)
                .map(i32::to_be_bytes)
                .map(|bytes| bytes.to_vec())
                .map_err(|_| out_of_range("integer"))?
        }
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other} to bytea"
            )));
        }
    };
    Ok(Value::Bytes(bytes))
}

pub(super) fn bytea_to_integer(bytes: &[u8], target: &str) -> Result<i64> {
    let width = match target {
        "smallint" => 2,
        "integer" => 4,
        _ => 8,
    };
    if bytes.len() > width {
        return Err(out_of_range(target));
    }
    let mut extended = [0_u8; 8];
    let offset = width - bytes.len();
    extended[8 - width + offset..].copy_from_slice(bytes);
    Ok(match width {
        2 => i64::from(i16::from_be_bytes([extended[6], extended[7]])),
        4 => i64::from(i32::from_be_bytes([
            extended[4],
            extended[5],
            extended[6],
            extended[7],
        ])),
        _ => i64::from_be_bytes(extended),
    })
}
