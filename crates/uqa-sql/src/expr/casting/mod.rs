//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL cast dispatch and scalar, numeric, and range conversion.

mod array;
mod binary_oid;
mod legacy_vector;
mod temporal;

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
#[expect(
    clippy::too_many_lines,
    reason = "cast matrix preserves source-target and error precedence"
)]
pub fn cast_value_from(v: &Value, ty: &str, source_ty: Option<&str>) -> Result<Value> {
    let normalized_type = ty.trim().to_ascii_lowercase();
    if normalized_type
        .strip_suffix("[]")
        .is_some_and(|element| element.trim() == "void" || element.trim() == "pg_catalog.void")
    {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: "type \"void[]\" does not exist".into(),
        });
    }
    if matches!(v, Value::Null) {
        return Ok(Value::Null);
    }
    let (base, modifier) = split_type_modifier(ty);
    let target = base
        .trim()
        .strip_prefix("pg_catalog.")
        .unwrap_or(base.trim());
    if matches!(v, Value::Void)
        && !matches!(
            target,
            "void"
                | "text"
                | "name"
                | "varchar"
                | "character varying"
                | "bpchar"
                | "character"
                | "char"
        )
    {
        return Err(undefined_cast("void", postgres_type_display_name(target)));
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
    match base {
        "void" | "pg_catalog.void" => {
            let source = canonical_cast_source(source_ty, v);
            if matches!(
                source.as_str(),
                "unknown" | "text" | "name" | "varchar" | "bpchar" | "void"
            ) {
                Ok(Value::Void)
            } else {
                Err(undefined_cast(postgres_type_display_name(&source), "void"))
            }
        }
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
        "regproc" | "regprocedure" | "regrole" | "regtype" if matches!(v, Value::Int(_)) => {
            Ok(v.clone())
        }
        "text"
        | "refcursor"
        | "pg_catalog.refcursor"
        | "name"
        | "regproc"
        | "regprocedure"
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
                (
                    Some(
                        "regproc" | "regprocedure" | "regclass" | "regnamespace" | "regrole"
                        | "regtype",
                    ),
                    Value::Int(0),
                ) => "-".into(),
                _ => value_to_string(v),
            };
            Ok(Value::Str(text))
        }
        "int2vector" | "pg_catalog.int2vector" => legacy_vector::cast_int2vector(v, source_ty),
        "oidvector" | "pg_catalog.oidvector" => legacy_vector::cast_oidvector(v, source_ty),
        "oid" | "pg_catalog.oid" => cast_oid(v, source_ty),
        "regclass" | "pg_catalog.regclass" => cast_regclass(v, source_ty),
        "regnamespace" | "pg_catalog.regnamespace" => cast_regnamespace(v, source_ty),
        "regrole" | "pg_catalog.regrole" => cast_regrole(v, source_ty),
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
        Value::Void => "void",
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

fn undefined_cast(source: &str, target: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42846".into(),
        message: format!("cannot cast type {source} to {target}"),
    }
}

fn postgres_type_display_name(name: &str) -> &str {
    match name {
        "int2" => "smallint",
        "int4" => "integer",
        "int8" => "bigint",
        "float4" => "real",
        "float8" => "double precision",
        "bool" => "boolean",
        "varchar" => "character varying",
        "bpchar" => "character",
        other => other,
    }
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

use array::cast_array_elements;
pub use array::{array_dimensions, parse_pg_array_literal};
use binary_oid::{
    bytea_to_integer, cast_bytea, cast_oid, cast_regclass, cast_regnamespace, cast_regrole,
    cast_xid,
};
use temporal::{cast_date, cast_temporal, TemporalCastTarget};

#[cfg(test)]
mod tests;
