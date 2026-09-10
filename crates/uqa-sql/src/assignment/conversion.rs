//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared-column assignment coercion for scalar, array, vector, and temporal values.

use super::AssignmentContext;
use crate::expr::{value_to_tensor, value_to_vector};
use crate::{ColumnType, SQLError};
use uqa_core::{ArrayValue, DecimalValue, TemporalValue, Value};

pub fn coerce_assignment_value(
    context: &dyn AssignmentContext,
    value: Value,
    target: &ColumnType,
    source: Option<&ColumnType>,
) -> Result<Value, SQLError> {
    if source.is_some_and(|source| same_domain_identity(source, target)) {
        return Ok(value);
    }
    let value = if target.is_character_string() {
        source
            .map(|source| crate::expr::format_regtype_value(&value, source, Some(context)))
            .transpose()?
            .flatten()
            .map(Value::Str)
            .unwrap_or(value)
    } else {
        value
    };
    convert_value_to_column_type_with_context(context, value, target)
}

fn same_domain_identity(source: &ColumnType, target: &ColumnType) -> bool {
    match (source, target) {
        (ColumnType::Domain { oid: source, .. }, ColumnType::Domain { oid: target, .. }) => {
            source == target
        }
        (ColumnType::Array(source), ColumnType::Array(target)) => {
            same_domain_identity(source, target)
        }
        _ => false,
    }
}

pub fn coerce_json_value(value: Value, jsonb: bool) -> Result<Value, SQLError> {
    crate::expr::cast_value(&value, if jsonb { "jsonb" } else { "json" })
}

pub fn convert_declared_value_to_column_type(
    context: &dyn AssignmentContext,
    value: Value,
    source_ty: &ColumnType,
    target_ty: &ColumnType,
) -> Result<Value, SQLError> {
    match (source_ty, target_ty) {
        (ColumnType::Domain { base, .. }, target) => {
            convert_declared_value_to_column_type(context, value, base, target)
        }
        (source, ColumnType::Domain { base, .. }) => {
            convert_declared_value_to_column_type(context, value, source, base)
        }
        (ColumnType::Array(source), ColumnType::Array(target)) => {
            let Value::Array(array) = value else {
                return Err(SQLError::TypeMismatch(format!(
                    "cannot cast a non-array value to {}[]",
                    column_type_name(target)
                )));
            };
            let source = array_scalar_type(source);
            let target = array_scalar_type(target);
            let converted =
                convert_declared_array_elements(context, array.elements(), source, target)?;
            ArrayValue::with_lower_bounds(converted, array.lower_bounds().to_vec())
                .map(Value::Array)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "multidimensional arrays must have matching dimensions".into(),
                    )
                })
        }
        (ColumnType::Range(source), ColumnType::Range(target)) if source == target => {
            crate::expr::cast_value_from(&value, target.range_name(), Some(source.range_name()))
        }
        (ColumnType::Range(source), ColumnType::Multirange(target)) if source == target => {
            crate::expr::cast_value_from(
                &value,
                target.multirange_name(),
                Some(source.range_name()),
            )
        }
        (ColumnType::Multirange(source), ColumnType::Multirange(target)) if source == target => {
            crate::expr::cast_value_from(
                &value,
                target.multirange_name(),
                Some(source.multirange_name()),
            )
        }
        (_, ColumnType::Range(_) | ColumnType::Multirange(_)) => {
            Err(SQLError::TypeMismatch(format!(
                "column cannot be cast automatically from type {} to type {}",
                column_type_name(source_ty),
                column_type_name(target_ty)
            )))
        }
        (source, ColumnType::Oid)
            if matches!(
                source,
                ColumnType::SmallInteger
                    | ColumnType::Integer
                    | ColumnType::BigInteger
                    | ColumnType::Oid
                    | ColumnType::Regproc
                    | ColumnType::Regprocedure
                    | ColumnType::Regclass
                    | ColumnType::Regnamespace
                    | ColumnType::Regrole
                    | ColumnType::Regtype
            ) =>
        {
            crate::expr::cast_value_from(&value, "oid", Some(column_type_name(source)))
        }
        (ColumnType::Xid, ColumnType::Xid) => Ok(value),
        (ColumnType::Bytea, ColumnType::Bytea) => Ok(value),
        (_, ColumnType::Oid | ColumnType::Xid | ColumnType::Bytea) => {
            Err(SQLError::TypeMismatch(format!(
                "column cannot be cast automatically from type {} to type {}",
                column_type_name(source_ty),
                column_type_name(target_ty)
            )))
        }
        _ => convert_value_to_column_type_with_context(context, value, target_ty),
    }
}

fn type_requires_catalog_resolution(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Regrole | ColumnType::Domain { .. } => true,
        ColumnType::Array(element) => type_requires_catalog_resolution(element),
        _ => false,
    }
}

pub fn convert_value_to_column_type_with_context(
    context: &dyn AssignmentContext,
    value: Value,
    ty: &ColumnType,
) -> Result<Value, SQLError> {
    if let Some(value) = super::domain::assign_domain_value(context, &value, ty)? {
        return Ok(value);
    }
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    if let ColumnType::Array(element) = ty {
        if type_requires_catalog_resolution(element) {
            return convert_catalog_array(context, value, element);
        }
    }
    if type_requires_catalog_resolution(ty) {
        return crate::expr::cast_value_with_type_resolution(
            &value,
            None,
            &ty.sql_name(),
            Some(context),
        );
    }
    convert_value_to_column_type(value, ty)
}

fn convert_catalog_array(
    context: &dyn AssignmentContext,
    value: Value,
    element: &ColumnType,
) -> Result<Value, SQLError> {
    let array = match value {
        Value::Array(array) => array,
        Value::Str(text) => crate::expr::parse_pg_array_literal(&text)?,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "expected an array, got {other:?}"
            )))
        }
    };
    let values = convert_catalog_array_elements(context, array.elements(), element)?;
    ArrayValue::with_lower_bounds(values, array.lower_bounds().to_vec())
        .map(Value::Array)
        .ok_or_else(|| {
            SQLError::TypeMismatch("multidimensional arrays must have matching dimensions".into())
        })
}

fn convert_catalog_array_elements(
    context: &dyn AssignmentContext,
    values: &[Value],
    element: &ColumnType,
) -> Result<Vec<Value>, SQLError> {
    let mut element = element;
    while let ColumnType::Array(nested) = element {
        element = nested;
    }
    values
        .iter()
        .map(|value| match value {
            Value::List(values) => {
                convert_catalog_array_elements(context, values, element).map(Value::List)
            }
            value => convert_value_to_column_type_with_context(context, value.clone(), element),
        })
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
pub fn convert_value_to_column_type(value: Value, ty: &ColumnType) -> Result<Value, SQLError> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    match ty {
        ColumnType::Named(name) => Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type \"{name}\" does not exist"),
        }),
        ColumnType::SmallInteger => crate::expr::cast_value(&value, "smallint"),
        ColumnType::Integer => crate::expr::cast_value(&value, "integer"),
        ColumnType::BigInteger => crate::expr::cast_value(&value, "bigint"),
        ColumnType::Oid | ColumnType::Xid => {
            let Value::Int(value) = crate::expr::cast_value(&value, "bigint")? else {
                unreachable!("bigint cast returned a non-integer value")
            };
            u32::try_from(value)
                .map(|value| Value::Int(i64::from(value)))
                .map_err(|_| {
                    SQLError::TypeMismatch(format!(
                        "value {value} is out of range for type {}",
                        column_type_name(ty)
                    ))
                })
        }
        ColumnType::Boolean => match value {
            Value::Bool(_) => Ok(value),
            Value::Str(text) => parse_boolean_text(&text)
                .map(Value::Bool)
                .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast `{text}` to boolean"))),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to boolean"
            ))),
        },
        ColumnType::Void => Ok(Value::Void),
        ColumnType::Text | ColumnType::RefCursor => Ok(Value::Str(value_to_text(&value))),
        ColumnType::Name => crate::expr::cast_value(&value, "name"),
        ColumnType::Uuid => crate::expr::cast_value(&value, "uuid"),
        ColumnType::Varchar(None) => Ok(Value::Str(value_to_text(&value))),
        ColumnType::Varchar(Some(length)) => convert_varying_character(value, *length),
        ColumnType::Bpchar => Ok(Value::FixedChar(value_to_text(&value))),
        ColumnType::Character(length) => {
            let length = usize::try_from(*length).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "character length {length} exceeds the platform addressable range"
                ))
            })?;
            let text = value_to_text(&value);
            let char_count = text.chars().count();
            let significant = if char_count > length {
                let retained = text.chars().take(length).collect::<String>();
                let discarded = text.chars().skip(length).collect::<String>();
                if !discarded.chars().all(|character| character == ' ') {
                    return Err(SQLError::Routine {
                        sqlstate: "22001".into(),
                        message: format!("value too long for type character({length})"),
                    });
                }
                retained
            } else {
                text
            };
            let padding = length.saturating_sub(significant.chars().count());
            let mut padded = significant;
            padded.extend(std::iter::repeat_n(' ', padding));
            Ok(Value::FixedChar(padded))
        }
        ColumnType::Real | ColumnType::DoublePrecision => {
            crate::expr::cast_value(&value, &ty.sql_name())
        }
        ColumnType::Numeric { precision, scale } => {
            let decimal = match value {
                Value::Decimal(d) => d,
                Value::Int(i) => DecimalValue::from_i64(i),
                Value::Float(f) => DecimalValue::from_f64_lossy(f).ok_or_else(|| {
                    SQLError::TypeMismatch(format!("cannot cast {f:?} to numeric"))
                })?,
                Value::Bool(b) => DecimalValue::from_bool(b),
                Value::Str(s) => DecimalValue::parse(&s).ok_or_else(|| {
                    SQLError::TypeMismatch(format!("cannot cast `{s}` to numeric"))
                })?,
                other => {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot cast {other:?} to numeric"
                    )));
                }
            };
            let rounded = match scale {
                Some(s) => decimal.round_to_scale(*s).ok_or_else(|| {
                    SQLError::TypeMismatch(format!("cannot round numeric to scale {s}"))
                })?,
                None => decimal,
            };
            if let Some(precision) = precision {
                let scale = scale.unwrap_or(0);
                if !rounded.fits_precision(*precision, scale) {
                    return Err(SQLError::TypeMismatch(format!(
                        "numeric field overflow: value {} exceeds precision {precision}, scale {scale}",
                        rounded.to_sql_string()
                    )));
                }
            }
            Ok(Value::Decimal(rounded))
        }
        ColumnType::Json => coerce_json_value(value, false),
        ColumnType::JsonB => coerce_json_value(value, true),
        ColumnType::Bytea => Ok(match value {
            Value::Bytes(_) => value,
            Value::Str(s) => Value::Bytes(s.into_bytes()),
            other => Value::Bytes(value_to_text(&other).into_bytes()),
        }),
        ColumnType::InternalChar => {
            let text = value_to_text(&value);
            if text.len() == 1 {
                Ok(Value::Str(text))
            } else {
                Err(SQLError::TypeMismatch(format!(
                    "value `{text}` must be exactly one byte for type \"char\""
                )))
            }
        }
        ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regclass
        | ColumnType::Regnamespace
        | ColumnType::Regtype
        | ColumnType::PgNodeTree
        | ColumnType::AclItem => Ok(match value {
            Value::Int(_) | Value::Str(_) => value,
            other => Value::Str(value_to_text(&other)),
        }),
        ColumnType::Regrole => match value {
            Value::Int(value) => u32::try_from(value)
                .map(|value| Value::Int(i64::from(value)))
                .map_err(|_| {
                    SQLError::TypeMismatch(format!(
                        "value {value} is out of range for type regrole"
                    ))
                }),
            Value::Str(_) | Value::FixedChar(_) => Err(SQLError::Internal(
                "regrole name conversion requires catalog resolution".into(),
            )),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to regrole"
            ))),
        },
        ColumnType::Int2Vector => convert_value_to_column_type(
            value,
            &ColumnType::Array(Box::new(ColumnType::SmallInteger)),
        ),
        ColumnType::OidVector => {
            convert_value_to_column_type(value, &ColumnType::Array(Box::new(ColumnType::Oid)))
        }
        ColumnType::AnyArray => match value {
            Value::Array(_) => Ok(value),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to anyarray"
            ))),
        },
        ColumnType::Record => match value {
            Value::Record(_) => Ok(value),
            Value::Row(values) => Ok(Value::Record(
                values
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| (format!("f{}", index + 1), value))
                    .collect(),
            )),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to record"
            ))),
        },
        ColumnType::Array(element_type) => {
            let array = match value {
                Value::Array(array) => array,
                Value::List(elements) => ArrayValue::try_new(elements).ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "multidimensional arrays must have matching dimensions".into(),
                    )
                })?,
                Value::Str(text) => crate::expr::parse_pg_array_literal(&text)?,
                other => {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot cast {other:?} to {}[]",
                        column_type_name(element_type)
                    )))
                }
            };
            let converted = convert_array_elements(array.elements(), element_type)?;
            ArrayValue::with_lower_bounds(converted, array.lower_bounds().to_vec())
                .map(Value::Array)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "multidimensional arrays must have matching dimensions".into(),
                    )
                })
        }
        ColumnType::Date
        | ColumnType::Time
        | ColumnType::TimePrecision(_)
        | ColumnType::TimeTz
        | ColumnType::TimeTzPrecision(_)
        | ColumnType::Timestamp
        | ColumnType::TimestampPrecision(_)
        | ColumnType::TimestampTz
        | ColumnType::TimestampTzPrecision(_)
        | ColumnType::Interval
        | ColumnType::IntervalWithFields { .. } => convert_temporal_value(value, ty),
        ColumnType::Range(subtype) => crate::expr::cast_value(&value, subtype.range_name()),
        ColumnType::Multirange(subtype) => {
            crate::expr::cast_value(&value, subtype.multirange_name())
        }
        ColumnType::Vector(dim) => {
            let vector = value_to_vector(&value)?;
            validate_vector_dimensions(*dim, vector.len())?;
            Ok(vector_to_value(vector))
        }
        ColumnType::Tensor(dim) => {
            let tensor = value_to_tensor(&value)?;
            for vector in &tensor {
                validate_vector_dimensions(*dim, vector.len())?;
            }
            Ok(Value::List(
                tensor.into_iter().map(vector_to_value).collect(),
            ))
        }
        ColumnType::Domain { base, .. } => convert_value_to_column_type(value, base),
    }
}

fn convert_array_elements(
    elements: &[Value],
    element_type: &ColumnType,
) -> Result<Vec<Value>, SQLError> {
    let element_type = array_scalar_type(element_type);
    elements
        .iter()
        .cloned()
        .map(|element| match element {
            Value::List(nested) => convert_array_elements(&nested, element_type).map(Value::List),
            scalar => convert_value_to_column_type(scalar, element_type),
        })
        .collect()
}

fn convert_declared_array_elements(
    context: &dyn AssignmentContext,
    elements: &[Value],
    source_type: &ColumnType,
    target_type: &ColumnType,
) -> Result<Vec<Value>, SQLError> {
    elements
        .iter()
        .cloned()
        .map(|element| match element {
            Value::List(nested) => {
                convert_declared_array_elements(context, &nested, source_type, target_type)
                    .map(Value::List)
            }
            scalar => {
                convert_declared_value_to_column_type(context, scalar, source_type, target_type)
            }
        })
        .collect()
}

fn array_scalar_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Array(element) = ty {
        ty = element;
    }
    ty
}

fn convert_varying_character(value: Value, length: u32) -> Result<Value, SQLError> {
    let length = usize::try_from(length).map_err(|_| {
        SQLError::TypeMismatch(format!(
            "character varying length {length} exceeds the platform addressable range"
        ))
    })?;
    let text = value_to_text(&value);
    if text.chars().count() <= length {
        return Ok(Value::Str(text));
    }
    let retained = text.chars().take(length).collect::<String>();
    let discarded = text.chars().skip(length).collect::<String>();
    if discarded.chars().all(|character| character == ' ') {
        Ok(Value::Str(retained))
    } else {
        Err(SQLError::Routine {
            sqlstate: "22001".into(),
            message: format!("value too long for type character varying({length})"),
        })
    }
}

fn vector_to_value(vector: Vec<f32>) -> Value {
    Value::List(
        vector
            .into_iter()
            .map(|value| Value::Float(f64::from(value)))
            .collect(),
    )
}

pub fn validate_vector_dimensions(expected: u32, actual: usize) -> Result<(), SQLError> {
    let expected = usize::try_from(expected).map_err(|_| {
        SQLError::TypeMismatch(format!(
            "declared vector dimension {expected} exceeds the platform usize range"
        ))
    })?;
    if actual == expected {
        Ok(())
    } else {
        Err(SQLError::VectorDimMismatch { expected, actual })
    }
}

pub use crate::catalog::type_metadata::column_type_name;

fn parse_boolean_text(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "on" | "1" => Some(true),
        "false" | "f" | "no" | "n" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn convert_temporal_value(value: Value, ty: &ColumnType) -> Result<Value, SQLError> {
    crate::expr::cast_value(&value, &ty.sql_name())
}

pub use crate::expr::value_to_text;

pub fn json_to_core_value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(d) = DecimalValue::parse(&n.to_string()) {
                Value::Decimal(d)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::Str(s),
        serde_json::Value::Array(items) => {
            Value::List(items.into_iter().map(json_to_core_value).collect())
        }
        serde_json::Value::Object(obj) => {
            if let Ok(temporal) =
                serde_json::from_value::<TemporalValue>(serde_json::Value::Object(obj.clone()))
            {
                return Value::Temporal(temporal);
            }
            Value::Map(
                obj.into_iter()
                    .map(|(k, v)| (k, json_to_core_value(v)))
                    .collect(),
            )
        }
    }
}

pub use crate::expr::core_value_to_json;

pub fn json_table_value_to_text(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Bool(b) => Value::Str(b.to_string()),
        serde_json::Value::Number(n) => Value::Str(n.to_string()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => Value::Str(value.to_string()),
    }
}

pub fn json_table_arg(value: &Value, name: &str) -> Result<serde_json::Value, SQLError> {
    match value {
        Value::Json(s) | Value::JsonB(s) | Value::Str(s) => {
            serde_json::from_str::<serde_json::Value>(s)
                .map_err(|e| SQLError::TypeMismatch(format!("{name}: invalid JSON: {e}")))
        }
        other => Ok(core_value_to_json(other)),
    }
}
