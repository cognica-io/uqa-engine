//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared-column assignment coercion for scalar, array, vector, and temporal values.

use super::AssignmentContext;
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

mod production;
pub use production::convert_value_to_column_type_with_control;

pub fn convert_value_to_column_type(value: Value, ty: &ColumnType) -> Result<Value, SQLError> {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    convert_value_to_column_type_with_control(control.finish(value, None)?, ty, &control)?
        .into_uncontrolled()
        .map_err(|_| SQLError::Internal("ordinary assignment production owner".into()))
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
