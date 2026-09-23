//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OID-family and binary representation conversion.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

use crate::error::{Result, SQLError};

use super::{canonical_cast_source_with_control, out_of_range, text_value, undefined_cast};

pub(super) fn cast_oid(
    value: &Value,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    let source = canonical_cast_source_with_control(source_ty, value, control)?;
    match (source.as_str(), value) {
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name",
            Value::Str(text) | Value::FixedChar(text),
        ) => parse_uint32_input(text, "oid", control),
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

pub(super) fn cast_regclass(
    value: &Value,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let source = canonical_cast_source_with_control(source_ty, value, control)?;
    match (source.as_str(), value) {
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name" | "regclass",
            Value::Str(text) | Value::FixedChar(text),
        ) => text_value(control.copy_text(text)?, false, control),
        (_, Value::Int(_)) => Ok(control.finish(
            cast_oid(value, source_ty, control)?,
            control.empty_reservation(),
        )?),
        _ => Err(undefined_cast(&source, "regclass")),
    }
}

pub(super) fn cast_regnamespace(
    value: &Value,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let source = canonical_cast_source_with_control(source_ty, value, control)?;
    match (source.as_str(), value) {
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name" | "regnamespace",
            Value::Str(text) | Value::FixedChar(text),
        ) => text_value(control.copy_text(text)?, false, control),
        (_, Value::Int(_)) => Ok(control.finish(
            cast_oid(value, source_ty, control)?,
            control.empty_reservation(),
        )?),
        _ => Err(undefined_cast(&source, "regnamespace")),
    }
}

pub(super) fn cast_regrole(
    value: &Value,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let source = canonical_cast_source_with_control(source_ty, value, control)?;
    match (source.as_str(), value) {
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name" | "regrole",
            Value::Str(text) | Value::FixedChar(text),
        ) => text_value(control.copy_text(text)?, false, control),
        (_, Value::Int(_)) => Ok(control.finish(
            cast_oid(value, source_ty, control)?,
            control.empty_reservation(),
        )?),
        _ => Err(undefined_cast(&source, "regrole")),
    }
}

pub(super) fn cast_xid(
    value: &Value,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    let source = canonical_cast_source_with_control(source_ty, value, control)?;
    match (source.as_str(), value) {
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name",
            Value::Str(text) | Value::FixedChar(text),
        ) => parse_uint32_input(text, "xid", control),
        ("xid", Value::Int(value)) => u32::try_from(*value)
            .map(|value| Value::Int(i64::from(value)))
            .map_err(|_| out_of_range("xid")),
        _ => Err(undefined_cast(&source, "xid")),
    }
}

pub(super) fn cast_bytea(
    value: &Value,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let source = canonical_cast_source_with_control(source_ty, value, control)?;
    match (source.as_str(), value) {
        ("bytea", Value::Bytes(_)) => Ok(control.copy_value(value)?),
        ("int2" | "int4" | "int8", Value::Int(value)) => {
            integer_to_bytea(*value, Some(&source), control)
        }
        (
            "unknown" | "text" | "varchar" | "bpchar" | "name",
            Value::Str(text) | Value::FixedChar(text),
        ) => parse_bytea_input(text, control),
        _ => Err(undefined_cast(&source, "bytea")),
    }
}

fn parse_bytea_input(text: &str, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    if let Some(hex) = text.strip_prefix("\\x") {
        if !hex.len().is_multiple_of(2) {
            return Err(invalid_bytea(
                "invalid hexadecimal data: odd number of digits",
            ));
        }
        let mut bytes = ProductionVec::new(*control);
        bytes.reserve(hex.len() / 2)?;
        for pair in hex.as_bytes().chunks_exact(2) {
            let hi = (pair[0] as char)
                .to_digit(16)
                .ok_or_else(|| invalid_bytea("invalid hexadecimal digit"))?;
            let lo = (pair[1] as char)
                .to_digit(16)
                .ok_or_else(|| invalid_bytea("invalid hexadecimal digit"))?;
            bytes.push_copy((hi * 16 + lo) as u8)?;
        }
        return finish_bytes(bytes.finish()?, control);
    }

    let input = text.as_bytes();
    let mut output = ProductionVec::new(*control);
    output.reserve(input.len())?;
    let mut index = 0;
    while index < input.len() {
        if input[index] != b'\\' {
            output.push_copy(input[index])?;
            index += 1;
            continue;
        }
        if input.get(index + 1) == Some(&b'\\') {
            output.push_copy(b'\\')?;
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
        output.push_copy((octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + (octal[2] - b'0'))?;
        index += 4;
    }
    finish_bytes(output.finish()?, control)
}

fn invalid_bytea(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: message.into(),
    }
}

fn parse_uint32_input(text: &str, target: &str, control: &ProductionControl<'_>) -> Result<Value> {
    for _ in text.as_bytes().chunks(4096) {
        control.check()?;
    }
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

fn integer_to_bytea(
    value: i64,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let (source, _) =
        crate::ast::split_type_modifier_with_control(source_ty.unwrap_or("integer"), control)?;
    let bytes = value.to_be_bytes();
    let slice = match &**source {
        "smallint" | "int2" | "pg_catalog.int2" => {
            i16::try_from(value).map_err(|_| out_of_range("smallint"))?;
            &bytes[6..]
        }
        "bigint" | "int8" | "bigserial" | "serial8" | "pg_catalog.int8" => &bytes[..],
        "integer" | "int" | "int4" | "serial" | "serial4" | "pg_catalog.int4" => {
            i32::try_from(value).map_err(|_| out_of_range("integer"))?;
            &bytes[4..]
        }
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other} to bytea"
            )))
        }
    };
    let mut output = ProductionVec::new(*control);
    output.reserve(slice.len())?;
    for byte in slice {
        output.push_copy(*byte)?;
    }
    finish_bytes(output.finish()?, control)
}

fn finish_bytes(
    bytes: Produced<Vec<u8>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let (bytes, memory) = bytes.into_parts();
    Ok(control.finish(Value::Bytes(bytes), memory)?)
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
