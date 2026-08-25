//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL casts and `PostgreSQL` array-literal parsing.

mod legacy_vector;

use super::{
    multirange_from_ranges, out_of_range, parse_json, parse_multirange, parse_range, to_decimal,
    to_f64, typed_json_value, value_to_json, value_to_string, vector_value_to_string, ArrayValue,
    Result, SQLError, TemporalValue, Value,
};
use crate::ast::RangeSubtype;

/// Cast a value to the named SQL type, mirroring `CAST(expr AS ty)`.
/// Types outside the engine's coercion surface return
/// [`SQLError::Unsupported`].
pub fn cast_value(v: &Value, ty: &str) -> Result<Value> {
    cast_value_from(v, ty, None)
}

/// Cast a value while preserving an explicitly declared source type when the runtime carrier erases it. `PostgreSQL` 18 integer-to-`bytea`/`oid` casts and `xid` cast rejection require the source's declared identity.
pub fn cast_value_from(v: &Value, ty: &str, source_ty: Option<&str>) -> Result<Value> {
    if matches!(v, Value::Null) {
        return Ok(Value::Null);
    }
    if let Some(elem_ty) = ty.strip_suffix("[]") {
        let source_elem_ty = source_ty
            .and_then(|source| source.trim().strip_suffix("[]"))
            .map(str::trim);
        let array = match v {
            Value::Array(array) => array.clone(),
            Value::Str(s) => parse_pg_array_literal(s)?,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "CAST AS {ty}: expected array, got {other:?}"
                )));
            }
        };
        let elements = cast_array_elements(array.elements(), elem_ty, source_elem_ty)?;
        return ArrayValue::with_lower_bounds(elements, array.lower_bounds().to_vec())
            .map(Value::Array)
            .ok_or_else(|| SQLError::TypeMismatch("array dimensions changed during cast".into()));
    }
    let (base, modifier) = split_type_modifier(ty);
    match base {
        "smallint" | "int2" | "pg_catalog.int2" => cast_integer(v, "smallint"),
        "integer" | "int" | "int4" | "serial" | "serial4" | "pg_catalog.int4" => {
            cast_integer(v, "integer")
        }
        "bigint" | "int8" | "bigserial" | "serial8" | "pg_catalog.int8" => {
            cast_integer(v, "bigint")
        }
        "real" | "float4" | "float8" | "double" | "double precision" => {
            Ok(Value::Float(to_f64(v)?))
        }
        "numeric" | "decimal" => {
            let value = to_decimal(v)?;
            if let Some(modifier) = modifier {
                let mut parts = modifier.split(',').map(str::trim);
                let precision: u32 = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| SQLError::TypeMismatch("bad numeric precision".into()))?;
                let scale: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let rounded = value
                    .round_to_scale(scale)
                    .ok_or_else(|| out_of_range("numeric"))?;
                if !rounded.fits_precision(precision, scale) {
                    return Err(SQLError::Routine {
                        sqlstate: "22003".into(),
                        message: format!(
                            "numeric field overflow: A field with precision {precision}, scale {scale} cannot hold value {}",
                            value.to_sql_string()
                        ),
                    });
                }
                return Ok(Value::Decimal(rounded));
            }
            Ok(Value::Decimal(value))
        }
        "regproc" | "regtype" if matches!(v, Value::Int(_)) => Ok(v.clone()),
        "text"
        | "refcursor"
        | "pg_catalog.refcursor"
        | "name"
        | "regproc"
        | "regtype"
        | "pg_node_tree"
        | "aclitem" => {
            let source = source_ty
                .map(str::trim)
                .map(|source| source.strip_prefix("pg_catalog.").unwrap_or(source));
            let text = match (source, v) {
                (Some("int2vector" | "oidvector"), _) => {
                    vector_value_to_string(v).unwrap_or_else(|| value_to_string(v))
                }
                (Some("regproc"), Value::Int(0)) => "-".into(),
                _ => value_to_string(v),
            };
            Ok(Value::Str(text))
        }
        "int2vector" | "pg_catalog.int2vector" => legacy_vector::cast_int2vector(v, source_ty),
        "oidvector" | "pg_catalog.oidvector" => legacy_vector::cast_oidvector(v, source_ty),
        "oid" | "pg_catalog.oid" => cast_oid(v, source_ty),
        "regclass" | "pg_catalog.regclass" => cast_regclass(v, source_ty),
        "regnamespace" | "pg_catalog.regnamespace" => cast_regnamespace(v, source_ty),
        "xid" | "pg_catalog.xid" => cast_xid(v, source_ty),
        "\"char\"" => {
            let text = value_to_string(v);
            let mut characters = text.chars();
            let Some(character) = characters.next() else {
                return Ok(Value::Str(String::new()));
            };
            if characters.next().is_some() || !character.is_ascii() {
                return Err(SQLError::TypeMismatch(format!(
                    "value too long for type character(1): {text:?}"
                )));
            }
            Ok(Value::Str(character.to_string()))
        }
        "uuid" => cast_uuid(v),
        // varchar(n): an explicit cast truncates to the declared length.
        "varchar" | "character varying" => {
            let text = value_to_string(v);
            let Some(modifier) = modifier else {
                return Ok(Value::Str(text));
            };
            let limit: usize = modifier
                .trim()
                .parse()
                .map_err(|_| SQLError::TypeMismatch(format!("bad length modifier {modifier}")))?;
            Ok(Value::Str(text.chars().take(limit).collect()))
        }
        // bpchar is physically blank-padded. Its implicit text coercion strips
        // those spaces, while a direct result retains them.
        "bpchar" if modifier.is_none() => Ok(Value::FixedChar(value_to_string(v))),
        "character" | "char" | "bpchar" => {
            let text = value_to_string(v);
            let limit: usize = match modifier {
                Some(modifier) => modifier.trim().parse().map_err(|_| {
                    SQLError::TypeMismatch(format!("bad length modifier {modifier}"))
                })?,
                None => 1,
            };
            if limit == 0 {
                return Err(SQLError::TypeMismatch(
                    "CHARACTER length must be greater than zero".into(),
                ));
            }
            let mut text = text.chars().take(limit).collect::<String>();
            text.extend(std::iter::repeat_n(
                ' ',
                limit.saturating_sub(text.chars().count()),
            ));
            Ok(Value::FixedChar(text))
        }
        "date" => cast_date(v, source_ty),
        "time" | "time without time zone" => cast_temporal(
            v,
            TemporalCastTarget::Time,
            TemporalValue::parse_time,
            "time",
        ),
        "timetz" | "time with time zone" => cast_temporal(
            v,
            TemporalCastTarget::TimeTz,
            TemporalValue::parse_time_tz,
            "time with time zone",
        ),
        "timestamp" | "datetime" | "timestamp without time zone" => cast_temporal(
            v,
            TemporalCastTarget::Timestamp,
            TemporalValue::parse_timestamp,
            "timestamp",
        ),
        "timestamptz" | "timestamp with time zone" => cast_temporal(
            v,
            TemporalCastTarget::TimestampTz,
            TemporalValue::parse_timestamp_tz,
            "timestamp with time zone",
        ),
        "interval" => cast_temporal(
            v,
            TemporalCastTarget::Interval,
            TemporalValue::parse_interval,
            "interval",
        ),
        "int4range" => cast_range(v, source_ty, RangeSubtype::Integer),
        "int8range" => cast_range(v, source_ty, RangeSubtype::BigInteger),
        "numrange" => cast_range(v, source_ty, RangeSubtype::Numeric),
        "daterange" => cast_range(v, source_ty, RangeSubtype::Date),
        "tsrange" => cast_range(v, source_ty, RangeSubtype::Timestamp),
        "tstzrange" => cast_range(v, source_ty, RangeSubtype::TimestampTz),
        "int4multirange" => cast_multirange(v, source_ty, RangeSubtype::Integer),
        "int8multirange" => cast_multirange(v, source_ty, RangeSubtype::BigInteger),
        "nummultirange" => cast_multirange(v, source_ty, RangeSubtype::Numeric),
        "datemultirange" => cast_multirange(v, source_ty, RangeSubtype::Date),
        "tsmultirange" => cast_multirange(v, source_ty, RangeSubtype::Timestamp),
        "tstzmultirange" => cast_multirange(v, source_ty, RangeSubtype::TimestampTz),
        "json" => {
            if let Value::Json(text) = v {
                return Ok(Value::Json(text.clone()));
            }
            if let Value::Str(text) | Value::FixedChar(text) = v {
                let _validated = parse_json(text)?;
                return Ok(Value::Json(text.clone()));
            }
            typed_json_value(&value_to_json(v), false)
        }
        "jsonb" => {
            if let Value::JsonB(text) = v {
                return Ok(Value::JsonB(text.clone()));
            }
            let parsed = match v {
                Value::Json(text) | Value::Str(text) | Value::FixedChar(text) => parse_json(text)?,
                other => value_to_json(other),
            };
            typed_json_value(&parsed, true)
        }
        "bytea" => cast_bytea(v, source_ty),
        "boolean" | "bool" => cast_boolean(v),
        other => Err(SQLError::Unsupported(format!("CAST AS {other}"))),
    }
}

fn cast_range(v: &Value, source_ty: Option<&str>, subtype: RangeSubtype) -> Result<Value> {
    let source = source_ty.map(canonical_type_name);
    if source.as_deref().is_some_and(|source| {
        source != subtype.range_name() && !matches!(source, "unknown" | "cstring")
    }) {
        return Err(undefined_cast(
            source.as_deref().unwrap_or("unknown"),
            subtype.range_name(),
        ));
    }
    let (Value::Str(text) | Value::FixedChar(text)) = v else {
        return Err(undefined_cast(
            source.as_deref().unwrap_or("unknown"),
            subtype.range_name(),
        ));
    };
    parse_range(text, subtype).map(|range| Value::Str(range.to_text()))
}

fn cast_multirange(v: &Value, source_ty: Option<&str>, subtype: RangeSubtype) -> Result<Value> {
    let source = source_ty.map(canonical_type_name);
    let (Value::Str(text) | Value::FixedChar(text)) = v else {
        return Err(undefined_cast(
            source.as_deref().unwrap_or("unknown"),
            subtype.multirange_name(),
        ));
    };
    match source.as_deref() {
        Some(source) if source == subtype.range_name() => {
            let range = parse_range(text, subtype)?;
            Ok(Value::Str(
                multirange_from_ranges(subtype, [range]).to_text(),
            ))
        }
        None | Some("unknown" | "cstring") => {
            parse_multirange(text, subtype).map(|multirange| Value::Str(multirange.to_text()))
        }
        Some(source) if source == subtype.multirange_name() => {
            parse_multirange(text, subtype).map(|multirange| Value::Str(multirange.to_text()))
        }
        Some(source) => Err(undefined_cast(source, subtype.multirange_name())),
    }
}

fn canonical_type_name(type_name: &str) -> String {
    let normalized = type_name.trim().to_ascii_lowercase();
    normalized
        .strip_prefix("pg_catalog.")
        .unwrap_or(&normalized)
        .to_string()
}

/// Apply `PostgreSQL` prefix `-` while retaining the operand's declared type.
pub fn negate_value(value: &Value, source_ty: Option<&str>) -> Result<Value> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    let source = canonical_cast_source(source_ty, value);
    match (source.as_str(), value) {
        ("int2", Value::Int(value)) => i16::try_from(*value)
            .ok()
            .and_then(i16::checked_neg)
            .map(|value| Value::Int(i64::from(value)))
            .ok_or_else(|| out_of_range("smallint")),
        ("int4", Value::Int(value)) => i32::try_from(*value)
            .ok()
            .and_then(i32::checked_neg)
            .map(|value| Value::Int(i64::from(value)))
            .ok_or_else(|| out_of_range("integer")),
        ("int8", Value::Int(value)) => value
            .checked_neg()
            .map(Value::Int)
            .ok_or_else(|| out_of_range("bigint")),
        ("float4" | "float8", Value::Float(value)) => Ok(Value::Float(-value)),
        ("numeric", Value::Decimal(value)) => uqa_core::DecimalValue::from_i64(0)
            .checked_sub(value)
            .map(Value::Decimal)
            .ok_or_else(|| out_of_range("numeric")),
        (
            "interval",
            Value::Temporal(TemporalValue::Interval {
                months,
                days,
                micros,
            }),
        ) => Ok(Value::Temporal(TemporalValue::Interval {
            months: months
                .checked_neg()
                .ok_or_else(|| out_of_range("interval"))?,
            days: days.checked_neg().ok_or_else(|| out_of_range("interval"))?,
            micros: micros
                .checked_neg()
                .ok_or_else(|| out_of_range("interval"))?,
        })),
        _ => Err(SQLError::TypeMismatch(format!(
            "operator does not exist: - {source}"
        ))),
    }
}

fn canonical_cast_source(source_ty: Option<&str>, value: &Value) -> String {
    let source = source_ty.unwrap_or(match value {
        Value::Str(_) | Value::FixedChar(_) => "unknown",
        Value::Int(_) => "integer",
        Value::Bool(_) => "boolean",
        Value::Float(_) => "double precision",
        Value::Decimal(_) => "numeric",
        Value::Bytes(_) => "bytea",
        Value::Temporal(TemporalValue::Interval { .. }) => "interval",
        Value::Temporal(_) => "timestamp",
        Value::Json(_) => "json",
        Value::JsonB(_) => "jsonb",
        Value::Array(_) => "anyarray",
        Value::List(_) => "anyarray",
        Value::Row(_) | Value::Record(_) => "record",
        Value::Map(_) => "jsonb",
        Value::Null => "unknown",
    });
    let (source, _) = split_type_modifier(source);
    let source = source
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let source = source.strip_prefix("pg_catalog.").unwrap_or(&source);
    match source {
        "smallint" | "int2" => "int2".into(),
        "integer" | "int" | "int4" | "serial" | "serial4" => "int4".into(),
        "bigint" | "int8" | "bigserial" | "serial8" => "int8".into(),
        "character varying" | "varchar" => "varchar".into(),
        "character" | "char" | "bpchar" => "bpchar".into(),
        "boolean" | "bool" => "bool".into(),
        "double" | "double precision" | "float8" => "float8".into(),
        "real" | "float4" => "float4".into(),
        other => other.into(),
    }
}

fn cast_oid(value: &Value, source_ty: Option<&str>) -> Result<Value> {
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

fn cast_regclass(value: &Value, source_ty: Option<&str>) -> Result<Value> {
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

fn cast_regnamespace(value: &Value, source_ty: Option<&str>) -> Result<Value> {
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

fn cast_xid(value: &Value, source_ty: Option<&str>) -> Result<Value> {
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

fn cast_bytea(value: &Value, source_ty: Option<&str>) -> Result<Value> {
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

fn undefined_cast(source: &str, target: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42846".into(),
        message: format!("cannot cast type {source} to {target}"),
    }
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

fn cast_uuid(value: &Value) -> Result<Value> {
    let text = match value {
        Value::Str(text) | Value::FixedChar(text) => text,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to uuid"
            )))
        }
    };
    super::uuid::canonicalize_uuid(text).map(Value::Str)
}

/// Split `varchar(10)` / `numeric(10,2)` into `("varchar", Some("10"))`.
pub(super) fn split_type_modifier(ty: &str) -> (&str, Option<&str>) {
    match (ty.find('('), ty.rfind(')')) {
        (Some(open), Some(close)) if close > open => {
            (ty[..open].trim_end(), Some(&ty[open + 1..close]))
        }
        _ => (ty, None),
    }
}

/// CAST to the integer family with `PostgreSQL` conversion rules:
/// float8 rounds half-to-even, numeric rounds half-away-from-zero,
/// strings must be integral text, and the result must fit the target
/// width.
pub(super) fn cast_integer(v: &Value, target: &str) -> Result<Value> {
    let n: i64 = match v {
        Value::Int(n) => *n,
        Value::Bool(b) => i64::from(*b),
        Value::Float(f) => {
            if !f.is_finite() {
                return Err(out_of_range(target));
            }
            let rounded = f.round_ties_even();
            // `i64::MAX as f64` rounds up to 2^63.  Comparing with `>` would
            // therefore admit 2^63 and Rust's float-to-int cast would silently
            // saturate it to `i64::MAX`.
            if rounded < i64::MIN as f64 || rounded >= 9_223_372_036_854_775_808.0 {
                return Err(out_of_range(target));
            }
            rounded as i64
        }
        Value::Decimal(d) => d
            .round_dp(0)
            .to_i64_trunc()
            .ok_or_else(|| out_of_range(target))?,
        Value::Str(s) | Value::FixedChar(s) => {
            s.trim().parse::<i64>().map_err(|_| SQLError::Routine {
                sqlstate: "22P02".into(),
                message: format!("invalid input syntax for type {target}: \"{s}\""),
            })?
        }
        Value::Bytes(bytes) => bytea_to_integer(bytes, target)?,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to {target}"
            )));
        }
    };
    let in_range = match target {
        "smallint" => i16::try_from(n).is_ok(),
        "integer" => i32::try_from(n).is_ok(),
        _ => true,
    };
    if !in_range {
        return Err(out_of_range(target));
    }
    Ok(Value::Int(n))
}

fn bytea_to_integer(bytes: &[u8], target: &str) -> Result<i64> {
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

/// CAST to boolean: strings follow `PostgreSQL`'s `parse_bool`
/// (prefixes of true/false/yes/no, on/off, 1/0); numbers are non-zero
/// tests.
pub(super) fn cast_boolean(v: &Value) -> Result<Value> {
    match v {
        Value::Bool(b) => Ok(Value::Bool(*b)),
        Value::Int(n) => Ok(Value::Bool(*n != 0)),
        Value::Float(f) => Ok(Value::Bool(*f != 0.0)),
        Value::Decimal(d) => Ok(Value::Bool(!d.is_zero())),
        Value::Str(s) | Value::FixedChar(s) => {
            let text = s.trim().to_ascii_lowercase();
            let matches_prefix = |word: &str| !text.is_empty() && word.starts_with(&text);
            let value = if matches_prefix("true") || matches_prefix("yes") || text == "1" {
                Some(true)
            } else if matches_prefix("false") || matches_prefix("no") || text == "0" {
                Some(false)
            } else if "on" == text {
                Some(true)
            } else if matches_prefix("off") && text.len() >= 2 {
                Some(false)
            } else {
                None
            };
            value.map(Value::Bool).ok_or_else(|| SQLError::Routine {
                sqlstate: "22P02".into(),
                message: format!("invalid input syntax for type boolean: \"{s}\""),
            })
        }
        other => Err(SQLError::TypeMismatch(format!(
            "cannot cast {other:?} to boolean"
        ))),
    }
}

/// Parse a `PostgreSQL` array literal (`{1,2,3}`, `{"a b",NULL}`,
/// `{{1,2},{3,4}}`) into nested lists of string/NULL values; the caller
/// casts elements.
pub fn parse_pg_array_literal(text: &str) -> Result<ArrayValue> {
    let mut parser = PgArrayLiteralParser::new(text);
    let (declared_dimensions, items) = parser.parse()?;
    if let Err(error) = array_shape(&items) {
        return Err(SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("malformed array literal: \"{text}\" ({})", error.message()),
        });
    }
    let array = ArrayValue::try_new(items).ok_or_else(|| SQLError::Routine {
        sqlstate: "22P02".into(),
        message: format!("malformed array literal: \"{text}\""),
    })?;
    let Some(declared_dimensions) = declared_dimensions else {
        return Ok(array);
    };
    let declared_lengths = declared_dimensions
        .iter()
        .map(|(_, length)| *length)
        .collect::<Vec<_>>();
    if declared_lengths != array.dimensions() {
        return Err(SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!(
                "malformed array literal: \"{text}\" (specified array dimensions do not match array contents)"
            ),
        });
    }
    let lower_bounds = declared_dimensions
        .into_iter()
        .map(|(lower, _)| lower)
        .collect();
    ArrayValue::with_lower_bounds(array.into_elements(), lower_bounds).ok_or_else(|| {
        SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("malformed array literal: \"{text}\""),
        }
    })
}

fn cast_array_elements(
    items: &[Value],
    element_type: &str,
    source_element_type: Option<&str>,
) -> Result<Vec<Value>> {
    items
        .iter()
        .map(|item| match item {
            Value::List(nested) => {
                cast_array_elements(nested, element_type, source_element_type).map(Value::List)
            }
            other => cast_value_from(other, element_type, source_element_type),
        })
        .collect()
}

pub(super) struct PgArrayLiteralParser<'a> {
    source: &'a str,
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

type ParsedArrayLiteral = (Option<Vec<(i32, usize)>>, Vec<Value>);

impl<'a> PgArrayLiteralParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            chars: source.chars().peekable(),
        }
    }

    fn parse(&mut self) -> Result<ParsedArrayLiteral> {
        self.skip_whitespace();
        let dimensions = self.parse_dimension_declaration()?;
        let items = self.parse_array()?;
        self.skip_whitespace();
        if self.chars.peek().is_some() {
            return Err(self.error("unexpected content after closing brace"));
        }
        Ok((dimensions, items))
    }

    fn parse_dimension_declaration(&mut self) -> Result<Option<Vec<(i32, usize)>>> {
        if self.chars.peek() != Some(&'[') {
            return Ok(None);
        }
        let mut dimensions = Vec::new();
        while self.chars.next_if_eq(&'[').is_some() {
            self.skip_whitespace();
            let lower = self.parse_dimension_bound()?;
            self.skip_whitespace();
            if self.chars.next() != Some(':') {
                return Err(self.error("array dimension must contain `:`"));
            }
            self.skip_whitespace();
            let upper = self.parse_dimension_bound()?;
            self.skip_whitespace();
            if self.chars.next() != Some(']') {
                return Err(self.error("array dimension is missing a closing `]`"));
            }
            if upper == i32::MAX {
                return Err(SQLError::Routine {
                    sqlstate: "54000".into(),
                    message: format!("array upper bound is too large: {upper}"),
                });
            }
            if upper < lower {
                return Err(SQLError::Routine {
                    sqlstate: "2202E".into(),
                    message: "upper bound cannot be less than lower bound".into(),
                });
            }
            let length = i64::from(upper)
                .checked_sub(i64::from(lower))
                .and_then(|difference| difference.checked_add(1))
                .and_then(|length| usize::try_from(length).ok())
                .ok_or_else(|| self.error("array dimension is out of range"))?;
            dimensions.push((lower, length));
            self.skip_whitespace();
        }
        if self.chars.next() != Some('=') {
            return Err(self.error("array dimensions must be followed by `=`"));
        }
        self.skip_whitespace();
        Ok(Some(dimensions))
    }

    fn parse_dimension_bound(&mut self) -> Result<i32> {
        let mut text = String::new();
        if self
            .chars
            .peek()
            .is_some_and(|character| matches!(character, '+' | '-'))
        {
            text.push(self.chars.next().expect("peeked array bound sign"));
        }
        while self.chars.peek().is_some_and(char::is_ascii_digit) {
            text.push(self.chars.next().expect("peeked array bound digit"));
        }
        if text.is_empty() || matches!(text.as_str(), "+" | "-") {
            return Err(self.error("array dimension bound must be an integer"));
        }
        text.parse()
            .map_err(|_| self.error("array dimension bound is out of range"))
    }

    fn parse_array(&mut self) -> Result<Vec<Value>> {
        if self.chars.next() != Some('{') {
            return Err(self.error("array value must start with `{`"));
        }
        self.skip_whitespace();
        if self.chars.next_if_eq(&'}').is_some() {
            return Ok(Vec::new());
        }

        let mut items = Vec::new();
        loop {
            self.skip_whitespace();
            items.push(self.parse_element()?);
            self.skip_whitespace();
            match self.chars.next() {
                Some(',') => {
                    self.skip_whitespace();
                    if matches!(self.chars.peek(), None | Some('}')) {
                        return Err(self.error("array contains a missing element"));
                    }
                }
                Some('}') => break,
                Some(_) => {
                    return Err(self.error("array elements must be separated by commas"));
                }
                None => return Err(self.error("array is missing a closing `}`")),
            }
        }
        Ok(items)
    }

    fn parse_element(&mut self) -> Result<Value> {
        match self.chars.peek() {
            Some('{') => self.parse_array().map(Value::List),
            Some('"') => self.parse_quoted_element().map(Value::Str),
            Some(',') | Some('}') | None => Err(self.error("array contains a missing element")),
            Some(_) => self.parse_unquoted_element(),
        }
    }

    fn parse_quoted_element(&mut self) -> Result<String> {
        let _opening_quote = self.chars.next();
        let mut value = String::new();
        loop {
            match self.chars.next() {
                Some('"') => return Ok(value),
                Some('\\') => value.push(
                    self.chars
                        .next()
                        .ok_or_else(|| self.error("quoted element ends with an escape"))?,
                ),
                Some(character) => value.push(character),
                None => return Err(self.error("array contains an unterminated quoted element")),
            }
        }
    }

    fn parse_unquoted_element(&mut self) -> Result<Value> {
        let mut value = String::new();
        let mut significant_len = 0;
        let mut was_escaped = false;
        while let Some(character) = self.chars.peek().copied() {
            match character {
                ',' | '}' => break,
                '{' | '"' => {
                    return Err(self.error("array contains an unescaped special character"));
                }
                '\\' => {
                    let _escape = self.chars.next();
                    let escaped = self
                        .chars
                        .next()
                        .ok_or_else(|| self.error("array element ends with an escape"))?;
                    value.push(escaped);
                    significant_len = value.len();
                    was_escaped = true;
                }
                _ => {
                    let _character = self.chars.next();
                    value.push(character);
                    if !character.is_whitespace() {
                        significant_len = value.len();
                    }
                }
            }
        }
        value.truncate(significant_len);
        if value.is_empty() {
            return Err(self.error("array contains a missing element"));
        }
        if !was_escaped && value.eq_ignore_ascii_case("null") {
            Ok(Value::Null)
        } else {
            Ok(Value::Str(value))
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .chars
            .next_if(|character| character.is_whitespace())
            .is_some()
        {}
    }

    fn error(&self, detail: &str) -> SQLError {
        SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!("malformed array literal: \"{}\" ({detail})", self.source),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArrayShapeError {
    MixedNesting,
    MismatchedDimensions,
}

impl ArrayShapeError {
    fn message(self) -> &'static str {
        match self {
            Self::MixedNesting => "cannot mix nested arrays and scalar elements",
            Self::MismatchedDimensions => "multidimensional arrays must have matching dimensions",
        }
    }
}

pub(super) fn array_shape(items: &[Value]) -> std::result::Result<Vec<usize>, ArrayShapeError> {
    let mut dimensions = vec![items.len()];
    let mut nested_shape: Option<Vec<usize>> = None;
    let mut has_scalar = false;
    for item in items {
        if let Value::List(nested) = item {
            let shape = array_shape(nested)?;
            if has_scalar {
                return Err(ArrayShapeError::MixedNesting);
            }
            if nested_shape
                .as_ref()
                .is_some_and(|expected| *expected != shape)
            {
                return Err(ArrayShapeError::MismatchedDimensions);
            }
            nested_shape = Some(shape);
        } else {
            if nested_shape.is_some() {
                return Err(ArrayShapeError::MixedNesting);
            }
            has_scalar = true;
        }
    }
    if let Some(shape) = nested_shape {
        dimensions.extend(shape);
    }
    Ok(dimensions)
}

/// Return every dimension of a rectangular array value.
///
/// `PostgreSQL` arrays cannot mix scalar and nested elements or contain
/// sub-arrays with different extents.
pub fn array_dimensions(items: &[Value]) -> Result<Vec<usize>> {
    array_shape(items).map_err(|error| SQLError::TypeMismatch(error.message().to_string()))
}

#[derive(Clone, Copy)]
pub(super) enum TemporalCastTarget {
    Date,
    Time,
    TimeTz,
    Timestamp,
    TimestampTz,
    Interval,
}

pub(super) fn cast_temporal(
    v: &Value,
    target: TemporalCastTarget,
    parse: fn(&str) -> Option<TemporalValue>,
    ty: &str,
) -> Result<Value> {
    match v {
        Value::Temporal(value) => cast_temporal_kind(value, target)
            .map(Value::Temporal)
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to {ty}"))),
        other => parse(&value_to_string(other))
            .map(Value::Temporal)
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to {ty}"))),
    }
}

fn cast_date(v: &Value, source_ty: Option<&str>) -> Result<Value> {
    match v {
        Value::Temporal(value) => cast_temporal_kind(value, TemporalCastTarget::Date)
            .map(Value::Temporal)
            .ok_or_else(|| undefined_cast(&canonical_cast_source(source_ty, v), "date")),
        Value::Str(text) | Value::FixedChar(text) => TemporalValue::try_parse_date(text)
            .map(Value::Temporal)
            .map_err(|error| {
                let field_overflow = matches!(
                    error.kind(),
                    chrono::format::ParseErrorKind::OutOfRange
                        | chrono::format::ParseErrorKind::Impossible
                );
                SQLError::Routine {
                    sqlstate: if field_overflow { "22008" } else { "22007" }.into(),
                    message: if field_overflow {
                        format!("date/time field value out of range: \"{text}\"")
                    } else {
                        format!("invalid input syntax for type date: \"{text}\"")
                    },
                }
            }),
        _ => Err(undefined_cast(&canonical_cast_source(source_ty, v), "date")),
    }
}

fn cast_temporal_kind(value: &TemporalValue, target: TemporalCastTarget) -> Option<TemporalValue> {
    const MICROS_PER_DAY: i64 = 86_400_000_000;
    match (target, value) {
        (TemporalCastTarget::Date, TemporalValue::Date { days }) => {
            Some(TemporalValue::Date { days: *days })
        }
        (
            TemporalCastTarget::Date,
            TemporalValue::Timestamp { micros } | TemporalValue::TimestampTz { micros },
        ) => Some(TemporalValue::Date {
            days: i32::try_from(micros.div_euclid(MICROS_PER_DAY)).ok()?,
        }),
        (TemporalCastTarget::Time, TemporalValue::Time { micros })
        | (TemporalCastTarget::Time, TemporalValue::TimeTz { micros, .. })
        | (
            TemporalCastTarget::Time,
            TemporalValue::Timestamp { micros } | TemporalValue::TimestampTz { micros },
        )
        | (TemporalCastTarget::Time, TemporalValue::Interval { micros, .. }) => {
            Some(TemporalValue::Time {
                micros: micros.rem_euclid(MICROS_PER_DAY),
            })
        }
        (
            TemporalCastTarget::TimeTz,
            TemporalValue::TimeTz {
                micros,
                offset_minutes,
            },
        ) => Some(TemporalValue::TimeTz {
            micros: *micros,
            offset_minutes: *offset_minutes,
        }),
        (TemporalCastTarget::TimeTz, TemporalValue::Time { micros })
        | (TemporalCastTarget::TimeTz, TemporalValue::TimestampTz { micros }) => {
            Some(TemporalValue::TimeTz {
                micros: micros.rem_euclid(MICROS_PER_DAY),
                offset_minutes: 0,
            })
        }
        (TemporalCastTarget::Timestamp, TemporalValue::Timestamp { micros })
        | (TemporalCastTarget::Timestamp, TemporalValue::TimestampTz { micros }) => {
            Some(TemporalValue::Timestamp { micros: *micros })
        }
        (TemporalCastTarget::Timestamp, TemporalValue::Date { days }) => {
            Some(TemporalValue::Timestamp {
                micros: i64::from(*days).checked_mul(MICROS_PER_DAY)?,
            })
        }
        (TemporalCastTarget::TimestampTz, TemporalValue::TimestampTz { micros })
        | (TemporalCastTarget::TimestampTz, TemporalValue::Timestamp { micros }) => {
            Some(TemporalValue::TimestampTz { micros: *micros })
        }
        (TemporalCastTarget::TimestampTz, TemporalValue::Date { days }) => {
            Some(TemporalValue::TimestampTz {
                micros: i64::from(*days).checked_mul(MICROS_PER_DAY)?,
            })
        }
        (
            TemporalCastTarget::Interval,
            TemporalValue::Interval {
                months,
                days,
                micros,
            },
        ) => Some(TemporalValue::Interval {
            months: *months,
            days: *days,
            micros: *micros,
        }),
        (TemporalCastTarget::Interval, TemporalValue::Time { micros }) => {
            Some(TemporalValue::Interval {
                months: 0,
                days: 0,
                micros: *micros,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_literal_rejects_postgresql_unrepresentable_upper_bound() {
        let error = parse_pg_array_literal("[2147483647:2147483647]={1}").unwrap_err();
        assert_eq!(error.sqlstate(), Some("54000"));
        assert_eq!(
            error.to_string(),
            "array upper bound is too large: 2147483647"
        );
        assert!(parse_pg_array_literal("[2147483646:2147483646]={1}").is_ok());
    }
    use uqa_core::DecimalValue;

    #[test]
    fn temporal_cross_casts_convert_the_carrier_kind() {
        let date = Value::Temporal(TemporalValue::parse_date("2020-01-02").unwrap());
        assert_eq!(
            cast_value(&date, "timestamp").unwrap(),
            Value::Temporal(TemporalValue::parse_timestamp("2020-01-02 00:00:00").unwrap())
        );
        let timestamp =
            Value::Temporal(TemporalValue::parse_timestamp("2020-01-02 03:04:05").unwrap());
        assert_eq!(
            cast_value(&timestamp, "date").unwrap(),
            Value::Temporal(TemporalValue::parse_date("2020-01-02").unwrap())
        );
        assert_eq!(
            cast_value(&timestamp, "time").unwrap(),
            Value::Temporal(TemporalValue::parse_time("03:04:05").unwrap())
        );
        let interval = Value::Temporal(TemporalValue::parse_interval("1 day 25:02:03").unwrap());
        assert_eq!(
            cast_value(&interval, "time").unwrap(),
            Value::Temporal(TemporalValue::parse_time("01:02:03").unwrap())
        );
    }

    #[test]
    fn uuid_cast_matches_postgresql_input_and_canonical_output() {
        for input in [
            "A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11",
            "a0eebc999c0b4ef8bb6d6bb9bd380a11",
            "{a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11}",
            "a0ee-bc99-9c0b-4ef8-bb6d-6bb9-bd38-0a11",
        ] {
            assert_eq!(
                cast_value(&Value::Str(input.into()), "uuid").unwrap(),
                Value::Str("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".into())
            );
        }
    }

    #[test]
    fn uuid_cast_rejects_postgresql_invalid_forms() {
        for input in [
            " a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11 ",
            "a0e-ebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
            "not-a-uuid",
        ] {
            let error = cast_value(&Value::Str(input.into()), "uuid").unwrap_err();
            assert_eq!(error.sqlstate(), Some("22P02"));
        }
    }

    #[test]
    fn oid_cast_preserves_postgresql_source_type_rules() {
        assert_eq!(
            cast_value_from(&Value::Int(-1), "oid", Some("smallint")).unwrap(),
            Value::Int(i64::from(u32::MAX))
        );
        assert_eq!(
            cast_value_from(&Value::Int(-1), "oid", Some("integer")).unwrap(),
            Value::Int(i64::from(u32::MAX))
        );
        assert_eq!(
            cast_value_from(&Value::Int(i64::from(u32::MAX)), "oid", Some("bigint")).unwrap(),
            Value::Int(i64::from(u32::MAX))
        );
        let error = cast_value_from(&Value::Int(-1), "oid", Some("bigint")).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"));
        assert_eq!(error.to_string(), "OID out of range");
        for source in ["boolean", "numeric", "double precision"] {
            let value = match source {
                "boolean" => Value::Bool(true),
                "numeric" => Value::Decimal(DecimalValue::from_i64(1)),
                _ => Value::Float(1.0),
            };
            let error = cast_value_from(&value, "oid", Some(source)).unwrap_err();
            assert_eq!(error.sqlstate(), Some("42846"));
        }
    }

    #[test]
    fn regclass_cast_preserves_bound_relation_names_and_oid_carriers() {
        assert_eq!(
            cast_value_from(
                &Value::Str("app.items".into()),
                "pg_catalog.regclass",
                Some("unknown")
            )
            .unwrap(),
            Value::Str("app.items".into())
        );
        assert_eq!(
            cast_value_from(&Value::Int(2205), "regclass", Some("oid")).unwrap(),
            Value::Int(2205)
        );
    }

    #[test]
    fn regproc_zero_uses_postgresql_dash_text_output() {
        assert_eq!(
            cast_value_from(&Value::Int(0), "text", Some("pg_catalog.regproc")).unwrap(),
            Value::Str("-".into())
        );
        assert_eq!(
            cast_value_from(&Value::Int(42), "text", Some("regproc")).unwrap(),
            Value::Str("42".into())
        );
    }

    #[test]
    fn oid_and_xid_text_input_use_postgresql_uint32_syntax() {
        for target in ["oid", "xid"] {
            assert_eq!(
                cast_value(&Value::Str("-1".into()), target).unwrap(),
                Value::Int(i64::from(u32::MAX))
            );
            assert_eq!(
                cast_value(&Value::Str(i32::MIN.to_string()), target).unwrap(),
                Value::Int(i64::from(i32::MIN as u32))
            );
            assert_eq!(
                cast_value(&Value::Str(u32::MAX.to_string()), target).unwrap(),
                Value::Int(i64::from(u32::MAX))
            );
            for input in ["-2147483649", "4294967296"] {
                let error = cast_value(&Value::Str(input.into()), target).unwrap_err();
                assert_eq!(error.sqlstate(), Some("22003"));
            }
            let error = cast_value(&Value::Str("1.0".into()), target).unwrap_err();
            assert_eq!(error.sqlstate(), Some("22P02"));
        }
    }

    #[test]
    fn xid_rejects_integer_and_oid_cast_sources() {
        for source in ["smallint", "integer", "bigint", "oid"] {
            let error = cast_value_from(&Value::Int(1), "xid", Some(source)).unwrap_err();
            assert_eq!(error.sqlstate(), Some("42846"));
        }
    }

    #[test]
    fn legacy_vector_text_casts_use_postgresql_space_separation() {
        let vector = Value::List(vec![Value::Int(23), Value::Int(25)]);
        assert_eq!(
            cast_value_from(&vector, "text", Some("oidvector")).unwrap(),
            Value::Str("23 25".into())
        );
        let stored = Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(3)]).unwrap());
        assert_eq!(
            cast_value_from(&stored, "text", Some("int2vector")).unwrap(),
            Value::Str("1 3".into())
        );
        assert_eq!(
            cast_value_from(
                &Value::List(Vec::new()),
                "text",
                Some("pg_catalog.int2vector")
            )
            .unwrap(),
            Value::Str(String::new())
        );
    }

    #[test]
    fn bytea_cast_preserves_postgresql_source_type_and_input_rules() {
        assert_eq!(
            cast_value_from(&Value::Int(-1), "bytea", Some("smallint")).unwrap(),
            Value::Bytes(vec![0xff, 0xff])
        );
        assert_eq!(
            cast_value_from(&Value::Int(-1), "bytea", Some("integer")).unwrap(),
            Value::Bytes(vec![0xff; 4])
        );
        assert_eq!(
            cast_value_from(&Value::Int(-1), "bytea", Some("bigint")).unwrap(),
            Value::Bytes(vec![0xff; 8])
        );
        assert_eq!(
            cast_value_from(&Value::Str("\\x6162".into()), "bytea", Some("text")).unwrap(),
            Value::Bytes(b"ab".to_vec())
        );
        assert_eq!(
            cast_value_from(&Value::Str("a\\\\b\\141".into()), "bytea", Some("text")).unwrap(),
            Value::Bytes(b"a\\ba".to_vec())
        );
        for (value, source) in [
            (Value::Bool(true), "boolean"),
            (Value::Decimal(DecimalValue::from_i64(1)), "numeric"),
            (Value::Float(1.0), "double precision"),
        ] {
            let error = cast_value_from(&value, "bytea", Some(source)).unwrap_err();
            assert_eq!(error.sqlstate(), Some("42846"));
        }
        for input in ["\\x1", "\\xzz", "\\9"] {
            let error = cast_value(&Value::Str(input.into()), "bytea").unwrap_err();
            assert_eq!(error.sqlstate(), Some("22023"));
        }
    }

    #[test]
    fn unary_minus_preserves_integer_width_and_overflow() {
        for (source, input, expected) in [
            ("smallint", 1_i64, -1_i64),
            ("integer", 1_i64, -1_i64),
            ("bigint", 1_i64, -1_i64),
        ] {
            assert_eq!(
                negate_value(&Value::Int(input), Some(source)).unwrap(),
                Value::Int(expected)
            );
        }
        for (source, minimum) in [
            ("smallint", i64::from(i16::MIN)),
            ("integer", i64::from(i32::MIN)),
            ("bigint", i64::MIN),
        ] {
            let error = negate_value(&Value::Int(minimum), Some(source)).unwrap_err();
            assert_eq!(error.sqlstate(), Some("22003"));
        }
    }

    #[test]
    fn unary_minus_preserves_interval_fields() {
        assert_eq!(
            negate_value(
                &Value::Temporal(TemporalValue::Interval {
                    months: 2,
                    days: -3,
                    micros: 4,
                }),
                Some("interval"),
            )
            .unwrap(),
            Value::Temporal(TemporalValue::Interval {
                months: -2,
                days: 3,
                micros: -4,
            })
        );
    }
}
