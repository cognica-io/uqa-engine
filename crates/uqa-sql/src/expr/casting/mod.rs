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

use super::conversion::value_to_string_with_control;
use super::{out_of_range, ArrayValue, Result, SQLError, TemporalValue, Value};
use crate::ast::RangeSubtype;
use uqa_core::memory::{Produced, ProductionControl, ProductionString, ProductionVec};

/// Cast a value to the named SQL type, mirroring `CAST(expr AS ty)`.
/// Types outside the engine's coercion surface return
/// [`SQLError::Unsupported`].
pub fn cast_value(v: &Value, ty: &str) -> Result<Value> {
    cast_value_from(v, ty, None)
}

/// Cast a value while preserving an explicitly declared source type when the runtime carrier erases it. `PostgreSQL` 18 integer-to-`bytea`/`oid` casts and `xid` cast rejection require the source's declared identity.
pub fn cast_value_from(v: &Value, ty: &str, source_ty: Option<&str>) -> Result<Value> {
    cast_value_from_with_control(v, ty, source_ty, &ProductionControl::uncontrolled())?
        .into_uncontrolled()
        .map_err(|_| SQLError::Internal("ordinary cast production owner".into()))
}

#[expect(
    clippy::too_many_lines,
    reason = "cast matrix preserves source-target and error precedence"
)]
pub fn cast_value_from_with_control(
    v: &Value,
    ty: &str,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    if ty.trim().strip_suffix("[]").is_some_and(|element| {
        element.trim().eq_ignore_ascii_case("void")
            || element.trim().eq_ignore_ascii_case("pg_catalog.void")
    }) {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: "type \"void[]\" does not exist".into(),
        });
    }
    if matches!(v, Value::Null) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let (base, modifier) = crate::ast::split_type_modifier_with_control(ty, control)?;
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
    if let Some(element_type) = ty.strip_suffix("[]") {
        let source_element_type = source_ty
            .and_then(|source| source.trim().strip_suffix("[]"))
            .map(str::trim)
            .or(match v {
                Value::LegacyVector(vector) => Some(match vector.kind() {
                    uqa_core::LegacyVectorKind::SmallInteger => "smallint",
                    uqa_core::LegacyVectorKind::Oid => "oid",
                }),
                _ => None,
            });
        let parsed;
        let array = match v {
            Value::Array(array) => array,
            Value::LegacyVector(vector) => vector.as_array(),
            Value::Str(text) => {
                parsed = array::parse_pg_array_literal_with_control(text, control)?;
                &parsed
            }
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "CAST AS {ty}: expected array, got {other:?}"
                )))
            }
        };
        let elements = array::cast_array_elements(
            array.elements(),
            element_type,
            source_element_type,
            control,
        )?;
        let normalize_empty = array.elements().is_empty()
            && !array::binary_compatible_elements(source_element_type, element_type, control)?;
        let mut bounds = ProductionVec::new(*control);
        bounds.reserve(array.lower_bounds().len())?;
        if !normalize_empty {
            for lower in array.lower_bounds() {
                bounds.push_copy(*lower)?;
            }
        }
        let array =
            ArrayValue::with_lower_bounds_with_control(elements, bounds.finish()?, control)?
                .ok_or_else(|| {
                    SQLError::TypeMismatch("array dimensions changed during cast".into())
                })?;
        let (array, memory) = array.into_parts();
        return Ok(control.finish(Value::Array(array), memory)?);
    }
    let value = match &**base {
        "void" | "pg_catalog.void" => {
            let source = canonical_cast_source_with_control(source_ty, v, control)?;
            if matches!(
                source.as_str(),
                "unknown" | "text" | "name" | "varchar" | "bpchar" | "void"
            ) {
                Ok(Value::Void)
            } else {
                Err(undefined_cast(postgres_type_display_name(&source), "void"))
            }
        }
        "smallint" | "int2" | "pg_catalog.int2" => cast_integer(v, "smallint", control),
        "integer" | "int" | "int4" | "serial" | "serial4" | "pg_catalog.int4" => {
            binary_oid::cast_integer_from(v, source_ty, control)
        }
        "bigint" | "int8" | "bigserial" | "serial8" | "pg_catalog.int8" => {
            cast_integer(v, "bigint", control)
        }
        "real" | "float4" | "pg_catalog.float4" => {
            super::floating::to_float_with_control(v, super::FloatWidth::Real, control)
                .map(Value::Float)
        }
        "float8" | "double" | "double precision" | "pg_catalog.float8" => {
            super::floating::to_float_with_control(v, super::FloatWidth::DoublePrecision, control)
                .map(Value::Float)
        }
        "numeric" | "decimal" => {
            let value = super::conversion::to_decimal_with_control(v, control)?;
            let value = if let Some(modifier) = modifier {
                let mut parts = modifier.split(',').map(str::trim);
                let precision: u32 = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or_else(|| SQLError::TypeMismatch("bad numeric precision".into()))?;
                let scale: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let rounded = value
                    .round_to_scale_with_control(scale, control)?
                    .ok_or_else(|| out_of_range("numeric"))?;
                if !rounded.fits_precision_with_control(precision, scale, control)? {
                    return Err(SQLError::Routine { sqlstate: "22003".into(), message: format!("numeric field overflow: A field with precision {precision}, scale {scale} cannot hold value {}", value.to_sql_string()) });
                }
                rounded
            } else {
                value
            };
            let (value, memory) = value.into_parts();
            return Ok(control.finish(Value::Decimal(value), memory)?);
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
                    match super::conversion::vector_value_to_string_with_control(v, control)? {
                        Some(text) => text,
                        None => value_to_string_with_control(v, control)?,
                    }
                }
                (
                    Some(
                        "regproc" | "regprocedure" | "regclass" | "regnamespace" | "regrole"
                        | "regtype",
                    ),
                    Value::Int(0),
                ) => control.copy_text("-")?,
                _ => cast_text(v, source_ty, control)?,
            };
            return text_value(text, false, control);
        }
        "int2vector" | "pg_catalog.int2vector" => {
            return legacy_vector::cast_int2vector(v, source_ty, control)
        }
        "oidvector" | "pg_catalog.oidvector" => {
            return legacy_vector::cast_oidvector(v, source_ty, control)
        }
        "oid" | "pg_catalog.oid" => cast_oid(v, source_ty, control),
        "regclass" | "pg_catalog.regclass" => return cast_regclass(v, source_ty, control),
        "regnamespace" | "pg_catalog.regnamespace" => {
            return cast_regnamespace(v, source_ty, control)
        }
        "regrole" | "pg_catalog.regrole" => return cast_regrole(v, source_ty, control),
        "xid" | "pg_catalog.xid" => cast_xid(v, source_ty, control),
        "\"char\"" => {
            let text = value_to_string_with_control(v, control)?;
            let mut characters = text.chars();
            if let Some(character) = characters.next() {
                if characters.next().is_some() || !character.is_ascii() {
                    return Err(SQLError::TypeMismatch(format!(
                        "value too long for type character(1): {:?}",
                        text.as_str()
                    )));
                }
            }
            return text_value(text, false, control);
        }
        "uuid" => return cast_uuid(v, control),
        "varchar" | "character varying" => {
            let text = cast_text(v, source_ty, control)?;
            let Some(modifier) = modifier else {
                return text_value(text, false, control);
            };
            let limit: usize = modifier
                .trim()
                .parse()
                .map_err(|_| SQLError::TypeMismatch(format!("bad length modifier {modifier}")))?;
            return character_value(text, limit, false, control);
        }
        "bpchar" if modifier.is_none() => {
            return text_value(cast_text(v, source_ty, control)?, true, control)
        }
        "character" | "char" | "bpchar" => {
            let text = cast_text(v, source_ty, control)?;
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
            return character_value(text, limit, true, control);
        }
        "date" => cast_date(v, source_ty, control),
        "time" | "time without time zone" => cast_temporal(
            v,
            TemporalCastTarget::Time,
            TemporalValue::parse_time_with_control,
            "time",
            modifier,
            control,
        ),
        "timetz" | "time with time zone" => cast_temporal(
            v,
            TemporalCastTarget::TimeTz,
            TemporalValue::parse_time_tz_with_control,
            "time with time zone",
            modifier,
            control,
        ),
        "timestamp" | "datetime" | "timestamp without time zone" => cast_temporal(
            v,
            TemporalCastTarget::Timestamp,
            TemporalValue::parse_timestamp_with_control,
            "timestamp",
            modifier,
            control,
        ),
        "timestamptz" | "timestamp with time zone" => cast_temporal(
            v,
            TemporalCastTarget::TimestampTz,
            TemporalValue::parse_timestamp_tz_with_control,
            "timestamp with time zone",
            modifier,
            control,
        ),
        "interval" => temporal::cast_interval(v, ty, control),
        name if name.starts_with("interval ") => temporal::cast_interval(v, ty, control),
        "int4range" => return cast_range(v, source_ty, RangeSubtype::Integer, control),
        "int8range" => return cast_range(v, source_ty, RangeSubtype::BigInteger, control),
        "numrange" => return cast_range(v, source_ty, RangeSubtype::Numeric, control),
        "daterange" => return cast_range(v, source_ty, RangeSubtype::Date, control),
        "tsrange" => return cast_range(v, source_ty, RangeSubtype::Timestamp, control),
        "tstzrange" => return cast_range(v, source_ty, RangeSubtype::TimestampTz, control),
        "int4multirange" => return cast_multirange(v, source_ty, RangeSubtype::Integer, control),
        "int8multirange" => {
            return cast_multirange(v, source_ty, RangeSubtype::BigInteger, control)
        }
        "nummultirange" => return cast_multirange(v, source_ty, RangeSubtype::Numeric, control),
        "datemultirange" => return cast_multirange(v, source_ty, RangeSubtype::Date, control),
        "tsmultirange" => return cast_multirange(v, source_ty, RangeSubtype::Timestamp, control),
        "tstzmultirange" => {
            return cast_multirange(v, source_ty, RangeSubtype::TimestampTz, control)
        }
        "json" => return super::json::cast_json_value_with_control(v, false, control),
        "jsonb" => return super::json::cast_json_value_with_control(v, true, control),
        "bytea" => return cast_bytea(v, source_ty, control),
        "boolean" | "bool" => cast_boolean(v),
        other => Err(SQLError::Unsupported(format!("CAST AS {other}"))),
    }?;
    Ok(control.finish(value, control.empty_reservation())?)
}

fn text_value(
    text: Produced<String>,
    fixed: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let (text, memory) = text.into_parts();
    Ok(control.finish(
        if fixed {
            Value::FixedChar(text)
        } else {
            Value::Str(text)
        },
        memory,
    )?)
}

fn character_value(
    text: Produced<String>,
    limit: usize,
    fixed: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let mut count = 0;
    let mut end = 0;
    for (index, character) in text.char_indices().take(limit) {
        control.check()?;
        count += 1;
        end = index + character.len_utf8();
    }
    let mut output = ProductionString::from_produced(text, *control)?;
    output.truncate(end)?;
    if fixed {
        for _ in count..limit {
            output.push(' ')?;
        }
    }
    text_value(output.finish()?, fixed, control)
}

fn cast_range(
    v: &Value,
    source_ty: Option<&str>,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let source = source_ty
        .map(|source| canonical_type_name(source, control))
        .transpose()?;
    let source = source.as_ref().map(|source| source.as_str());
    if source.is_some_and(|source| {
        source != subtype.range_name() && !matches!(source, "unknown" | "cstring")
    }) {
        return Err(undefined_cast(
            source.unwrap_or("unknown"),
            subtype.range_name(),
        ));
    }
    let (Value::Str(text) | Value::FixedChar(text)) = v else {
        return Err(undefined_cast(
            source.unwrap_or("unknown"),
            subtype.range_name(),
        ));
    };
    text_value(
        super::range::canonical_range_text_with_control(text, subtype, control)?,
        false,
        control,
    )
}

fn cast_multirange(
    v: &Value,
    source_ty: Option<&str>,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let source = source_ty
        .map(|source| canonical_type_name(source, control))
        .transpose()?;
    let source = source.as_ref().map(|source| source.as_str());
    let (Value::Str(text) | Value::FixedChar(text)) = v else {
        return Err(undefined_cast(
            source.unwrap_or("unknown"),
            subtype.multirange_name(),
        ));
    };
    let text = match source {
        Some(source) if source == subtype.range_name() => {
            super::range::canonical_range_as_multirange_text_with_control(text, subtype, control)?
        }
        None | Some("unknown" | "cstring") => {
            super::range::canonical_multirange_text_with_control(text, subtype, control)?
        }
        Some(source) if source == subtype.multirange_name() => {
            super::range::canonical_multirange_text_with_control(text, subtype, control)?
        }
        Some(source) => return Err(undefined_cast(source, subtype.multirange_name())),
    };
    text_value(text, false, control)
}

fn canonical_type_name(
    type_name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut normalized = ProductionString::new(*control);
    for character in type_name.trim().chars() {
        normalized.push(character.to_ascii_lowercase())?;
    }
    Ok(control.copy_text(
        normalized
            .strip_prefix("pg_catalog.")
            .unwrap_or(&normalized),
    )?)
}

/// Apply `PostgreSQL` prefix `-` while retaining the operand's declared type.
pub fn negate_value(value: &Value, source_ty: Option<&str>) -> Result<Value> {
    negate_value_with_control(value, source_ty, &ProductionControl::uncontrolled())?
        .into_uncontrolled()
        .map_err(|_| SQLError::Internal("ordinary negation owner".into()))
}

pub fn negate_value_with_control(
    value: &Value,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    if matches!(value, Value::Null) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let source = canonical_cast_source_with_control(source_ty, value, control)?;
    let result = match (source.as_str(), value) {
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
        ("numeric", Value::Decimal(value)) => {
            let (value, memory) = value.negated_with_control(control)?.into_parts();
            return Ok(control.finish(Value::Decimal(value), memory)?);
        }
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
            "operator does not exist: - {}",
            source.as_str()
        ))),
    }?;
    Ok(control.finish(result, control.empty_reservation())?)
}

fn canonical_cast_source_with_control(
    source_ty: Option<&str>,
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
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
        Value::LegacyVector(vector) => vector.kind().type_name(),
        Value::List(_) => "anyarray",
        Value::Row(_) | Value::Record(_) => "record",
        Value::Map(_) => "jsonb",
        Value::Null => "unknown",
        Value::Void => "void",
    });
    let (source, _) = crate::ast::split_type_modifier_with_control(source, control)?;
    let mut normalized = ProductionString::new(*control);
    for (index, word) in source.split_whitespace().enumerate() {
        if index != 0 {
            normalized.push(' ')?;
        }
        for character in word.chars() {
            normalized.push(character.to_ascii_lowercase())?;
        }
    }
    let source = normalized
        .strip_prefix("pg_catalog.")
        .unwrap_or(&normalized);
    let canonical = match source {
        "smallint" | "int2" => "int2",
        "integer" | "int" | "int4" | "serial" | "serial4" => "int4",
        "bigint" | "int8" | "bigserial" | "serial8" => "int8",
        "character varying" | "varchar" => "varchar",
        "character" | "char" | "bpchar" => "bpchar",
        "boolean" | "bool" => "bool",
        "double" | "double precision" | "float8" => "float8",
        "real" | "float4" => "float4",
        other => other,
    };
    Ok(control.copy_text(canonical)?)
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

fn cast_text(
    value: &Value,
    source: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    if let (Value::Float(value), Some(source)) = (value, source) {
        let source = crate::ast::ColumnType::from_sql_name_with_control(source, control);
        match source {
            Ok(source) if matches!(&*source, crate::ast::ColumnType::Real) => {
                return super::floating::format_real_with_control(*value as f32, control)
            }
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
            _ => {}
        }
    }
    value_to_string_with_control(value, control)
}

fn cast_uuid(value: &Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    let text = match value {
        Value::Str(text) | Value::FixedChar(text) => text,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to uuid"
            )))
        }
    };
    text_value(
        super::uuid::canonicalize_uuid_with_control(text, control)?,
        false,
        control,
    )
}

/// CAST to the integer family with `PostgreSQL` conversion rules:
/// float8 rounds half-to-even, numeric rounds half-away-from-zero,
/// strings must be integral text, and the result must fit the target
/// width.
pub(super) fn cast_integer(
    v: &Value,
    target: &str,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    control.check()?;
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
            .round_to_scale_with_control(0, control)?
            .ok_or_else(|| out_of_range(target))?
            .to_i64_trunc_with_control(control)?
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
            let text = s.trim();
            let matches_prefix = |word: &str| {
                !text.is_empty()
                    && word
                        .get(..text.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(text))
            };
            let value = if matches_prefix("true") || matches_prefix("yes") || text == "1" {
                Some(true)
            } else if matches_prefix("false") || matches_prefix("no") || text == "0" {
                Some(false)
            } else if text.eq_ignore_ascii_case("on") {
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

pub use array::{array_dimensions, parse_pg_array_literal, parse_pg_array_literal_with_control};
use binary_oid::{
    bytea_to_integer, cast_bytea, cast_oid, cast_regclass, cast_regnamespace, cast_regrole,
    cast_xid,
};
use temporal::{cast_date, cast_temporal, TemporalCastTarget};

#[cfg(test)]
mod production_tests;
#[cfg(test)]
mod tests;
