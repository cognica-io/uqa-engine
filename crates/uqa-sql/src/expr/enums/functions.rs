//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The `anyenum` support functions bound to one concrete enum type.

use std::cmp::Ordering;

use uqa_core::{EnumValue, Value};

use super::{enum_endpoint, enum_label, enum_range, EnumLabelCatalog};
use crate::ast::EnumFunctionOperation;
use crate::error::{Result, SQLError};
use crate::expr::hashing::hash_bytes_uint32_extended;

fn enum_argument(value: &Value, type_oid: u32) -> Result<Option<&EnumValue>> {
    match value {
        Value::Null => Ok(None),
        Value::Enum(label) if label.type_oid() == type_oid => Ok(Some(label)),
        other => Err(SQLError::Internal(format!(
            "enum support function bound to type OID {type_oid} received {other:?}"
        ))),
    }
}

fn ordering_value(ordering: Ordering) -> Value {
    Value::Int(match ordering {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    })
}

/// Evaluate one bound enum support function. Strict operations return NULL for any NULL argument before reading the catalog.
pub fn enum_function_value(
    catalog: Option<&dyn EnumLabelCatalog>,
    operation: EnumFunctionOperation,
    type_oid: u32,
    arguments: &[Value],
) -> Result<Value> {
    let arity = match operation {
        EnumFunctionOperation::First
        | EnumFunctionOperation::Last
        | EnumFunctionOperation::Range
        | EnumFunctionOperation::Hash => 1,
        _ => 2,
    };
    if arguments.len() != arity {
        return Err(SQLError::Internal(format!(
            "{} received {} arguments",
            operation.label(),
            arguments.len()
        )));
    }
    if operation.is_strict() && arguments.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(Value::Null);
    }
    let first = enum_argument(&arguments[0], type_oid)?;
    let lost_argument =
        || SQLError::Internal(format!("{} lost a strict argument", operation.label()));
    match operation {
        EnumFunctionOperation::First => enum_endpoint(catalog, type_oid, false),
        EnumFunctionOperation::Last => enum_endpoint(catalog, type_oid, true),
        EnumFunctionOperation::Range => enum_range(catalog, type_oid, None, None),
        EnumFunctionOperation::BoundedRange => enum_range(
            catalog,
            type_oid,
            first,
            enum_argument(&arguments[1], type_oid)?,
        ),
        // `hashenum` and `hashenumextended` hash the label OID with `hash_uint32` and `hash_uint32_extended`.
        EnumFunctionOperation::Hash => {
            let oid = enum_label(catalog, first.ok_or_else(lost_argument)?)?.oid;
            let hash = hash_bytes_uint32_extended(oid, 0) as u32;
            Ok(Value::Int(i64::from(hash as i32)))
        }
        EnumFunctionOperation::ExtendedHash => {
            let Value::Int(seed) = arguments[1] else {
                return Err(SQLError::Internal(format!(
                    "hashenumextended received seed {:?}",
                    arguments[1]
                )));
            };
            let oid = enum_label(catalog, first.ok_or_else(lost_argument)?)?.oid;
            Ok(Value::Int(
                hash_bytes_uint32_extended(oid, seed as u64) as i64
            ))
        }
        _ => {
            let (Some(left), Some(right)) = (first, enum_argument(&arguments[1], type_oid)?) else {
                return Err(lost_argument());
            };
            comparison_value(operation, left, right)
        }
    }
}

/// The comparison support functions, which order two values of the bound type by label key.
fn comparison_value(
    operation: EnumFunctionOperation,
    left: &EnumValue,
    right: &EnumValue,
) -> Result<Value> {
    let ordering = left.key().cmp(right.key());
    Ok(match operation {
        EnumFunctionOperation::Compare => ordering_value(ordering),
        EnumFunctionOperation::Equal => Value::Bool(ordering.is_eq()),
        EnumFunctionOperation::NotEqual => Value::Bool(ordering.is_ne()),
        EnumFunctionOperation::Less => Value::Bool(ordering.is_lt()),
        EnumFunctionOperation::Greater => Value::Bool(ordering.is_gt()),
        EnumFunctionOperation::LessEqual => Value::Bool(ordering.is_le()),
        EnumFunctionOperation::GreaterEqual => Value::Bool(ordering.is_ge()),
        EnumFunctionOperation::Smaller => {
            Value::Enum(if ordering.is_le() { left } else { right }.clone())
        }
        EnumFunctionOperation::Larger => {
            Value::Enum(if ordering.is_ge() { left } else { right }.clone())
        }
        other => {
            return Err(SQLError::Internal(format!(
                "{} is not an enum comparison",
                other.label()
            )))
        }
    })
}
