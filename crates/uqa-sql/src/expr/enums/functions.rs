//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The `anyenum` support functions bound to one concrete enum type.

use std::cmp::Ordering;

use uqa_core::memory::{Produced, ProductionControl};
use uqa_core::Value;

use super::{
    enum_endpoint_with_control, physical, range_by_oid_with_control, EnumComparisonState,
    EnumLabelCatalog,
};
use crate::ast::EnumFunctionOperation;
use crate::error::{Result, SQLError};
use crate::expr::hashing::hash_bytes_uint32_extended;

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
    enum_function_value_with_state(catalog, operation, type_oid, arguments, None)
}

/// Evaluate with the calling expression's retained comparison state. Equality,
/// hashes and type-only operations do not initialize or inspect that state.
pub fn enum_function_value_with_state(
    catalog: Option<&dyn EnumLabelCatalog>,
    operation: EnumFunctionOperation,
    type_oid: u32,
    arguments: &[Value],
    state: Option<&EnumComparisonState>,
) -> Result<Value> {
    enum_function_value_with_control(
        catalog,
        operation,
        type_oid,
        arguments,
        state,
        &ProductionControl::uncontrolled(),
    )
    .map(|value| {
        value
            .into_uncontrolled()
            .expect("ordinary enum support result")
    })
}

/// Preserve the caller's catalog and function state while admitting selected values, label keys, range elements and array metadata before allocation.
pub fn enum_function_value_with_control(
    catalog: Option<&dyn EnumLabelCatalog>,
    operation: EnumFunctionOperation,
    type_oid: u32,
    arguments: &[Value],
    state: Option<&EnumComparisonState>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
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
        return plain(Value::Null, control);
    }
    let value = match operation {
        EnumFunctionOperation::First => {
            return enum_endpoint_with_control(catalog, type_oid, false, control)
        }
        EnumFunctionOperation::Last => {
            return enum_endpoint_with_control(catalog, type_oid, true, control)
        }
        EnumFunctionOperation::Range => {
            return range_by_oid_with_control(catalog, type_oid, None, None, control)
        }
        EnumFunctionOperation::BoundedRange => {
            return range_by_oid_with_control(
                catalog,
                type_oid,
                physical::oid(catalog, &arguments[0])?,
                physical::oid(catalog, &arguments[1])?,
                control,
            )
        }
        // `hashenum` and `hashenumextended` hash the label OID with `hash_uint32` and `hash_uint32_extended`.
        EnumFunctionOperation::Hash => {
            let oid = strict_oid(catalog, &arguments[0])?;
            let hash = hash_bytes_uint32_extended(oid, 0) as u32;
            Value::Int(i64::from(hash as i32))
        }
        EnumFunctionOperation::ExtendedHash => {
            let Value::Int(seed) = arguments[1] else {
                return Err(SQLError::Internal(format!(
                    "hashenumextended received seed {:?}",
                    arguments[1]
                )));
            };
            let oid = strict_oid(catalog, &arguments[0])?;
            Value::Int(hash_bytes_uint32_extended(oid, seed as u64) as i64)
        }
        _ => {
            return comparison_value(
                catalog,
                operation,
                &arguments[0],
                &arguments[1],
                state,
                control,
            )
        }
    };
    plain(value, control)
}

fn plain(value: Value, control: &ProductionControl<'_>) -> Result<Produced<Value>> {
    Ok(control.finish(value, control.empty_reservation())?)
}

fn strict_oid(catalog: Option<&dyn EnumLabelCatalog>, value: &Value) -> Result<u32> {
    physical::oid(catalog, value)?
        .ok_or_else(|| SQLError::Internal("enum support function lost a strict argument".into()))
}

/// Equality reads OIDs directly; ordering consults label order only on the slow path.
fn comparison_value(
    catalog: Option<&dyn EnumLabelCatalog>,
    operation: EnumFunctionOperation,
    left: &Value,
    right: &Value,
    state: Option<&EnumComparisonState>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    if operation == EnumFunctionOperation::Equal {
        return plain(Value::Bool(physical::equal(catalog, left, right)?), control);
    }
    if operation == EnumFunctionOperation::NotEqual {
        return plain(
            Value::Bool(!physical::equal(catalog, left, right)?),
            control,
        );
    }
    let ordering = physical::compare(catalog, left, right, state)?;
    let value = match operation {
        EnumFunctionOperation::Compare => ordering_value(ordering),
        EnumFunctionOperation::Less => Value::Bool(ordering.is_lt()),
        EnumFunctionOperation::Greater => Value::Bool(ordering.is_gt()),
        EnumFunctionOperation::LessEqual => Value::Bool(ordering.is_le()),
        EnumFunctionOperation::GreaterEqual => Value::Bool(ordering.is_ge()),
        EnumFunctionOperation::Smaller => {
            return Ok(control.copy_value(if ordering.is_lt() { left } else { right })?)
        }
        EnumFunctionOperation::Larger => {
            return Ok(control.copy_value(if ordering.is_gt() { left } else { right })?)
        }
        other => {
            return Err(SQLError::Internal(format!(
                "{} is not an enum comparison",
                other.label()
            )))
        }
    };
    plain(value, control)
}

#[cfg(test)]
mod tests;
