//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical enum operations read admitted OIDs without repeating enum input or output.

use std::cmp::Ordering;
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

use uqa_core::Value;

use super::{enum_label_oid, type_name, EnumLabelCatalog};
use crate::ast::BinaryOp;
use crate::error::{Result, SQLError};

#[cfg(test)]
mod tests;

/// The type cache of one prepared enum comparison call. The execution owner
/// creates a fresh state for each call site, retaining it across that site's rows.
#[derive(Debug, Default)]
pub struct EnumComparisonState {
    type_oid: AtomicU32,
}

impl EnumComparisonState {
    fn cached_type(&self) -> Option<u32> {
        match self.type_oid.load(AtomicOrdering::Relaxed) {
            0 => None,
            oid => Some(oid),
        }
    }

    fn remember_type(&self, oid: u32) -> u32 {
        self.type_oid
            .compare_exchange(0, oid, AtomicOrdering::Relaxed, AtomicOrdering::Relaxed)
            .map_or_else(|existing| existing, |_| oid)
    }
}

/// Classify an already-bound enum value and read its physical equality/hash
/// identity. A retained label need not exist: these operators never call enum output.
pub fn comparison_identity(catalog: &dyn EnumLabelCatalog, value: &Value) -> Result<Option<u32>> {
    match value {
        Value::Enum(_) => oid(Some(catalog), value),
        Value::Datum(datum) if catalog.enum_type_labels(datum.type_oid())?.is_some() => {
            super::super::datums::enum_label_oid(datum).map(Some)
        }
        _ => Ok(None),
    }
}

/// Evaluate a scalar enum operator against physical identities. Other operand
/// types remain with the ordinary SQL comparison owner; no label output occurs.
pub fn eval_comparison(
    op: BinaryOp,
    left: &Value,
    right: &Value,
    catalog: Option<&dyn EnumLabelCatalog>,
    state: Option<&EnumComparisonState>,
) -> Result<Option<Value>> {
    if matches!(
        op,
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
    ) {
        return Ok(None);
    }
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return Ok(Some(Value::Null));
    }
    let Some(catalog) = catalog else {
        return Ok(None);
    };
    let (Some(left), Some(right)) = (
        comparison_identity(catalog, left)?,
        comparison_identity(catalog, right)?,
    ) else {
        return Ok(None);
    };
    let value = match op {
        BinaryOp::Equal => left == right,
        BinaryOp::NotEqual => left != right,
        _ => {
            let order = compare_oids(Some(catalog), left, right, state)?;
            match op {
                BinaryOp::Less => order.is_lt(),
                BinaryOp::LessEqual => order.is_le(),
                BinaryOp::Greater => order.is_gt(),
                BinaryOp::GreaterEqual => order.is_ge(),
                _ => unreachable!("arithmetic has no enum comparison"),
            }
        }
    };
    Ok(Some(Value::Bool(value)))
}

pub(super) fn oid(catalog: Option<&dyn EnumLabelCatalog>, value: &Value) -> Result<Option<u32>> {
    match value {
        Value::Null => Ok(None),
        Value::Enum(label) => label
            .label_oid()
            .map(Ok)
            .unwrap_or_else(|| enum_label_oid(catalog, label))
            .map(Some),
        Value::Datum(datum) => super::super::datums::enum_label_oid(datum).map(Some),
        other => Err(SQLError::Internal(format!(
            "enum support function received {other:?}"
        ))),
    }
}

pub(super) fn equal(
    catalog: Option<&dyn EnumLabelCatalog>,
    left: &Value,
    right: &Value,
) -> Result<bool> {
    if let Some(order) = native_order(left, right) {
        return Ok(order.is_eq());
    }
    Ok(oid(catalog, left)? == oid(catalog, right)?)
}

fn native_order(left: &Value, right: &Value) -> Option<Ordering> {
    match (left, right) {
        (Value::Enum(left), Value::Enum(right)) if left.type_oid() == right.type_oid() => {
            Some(left.key().cmp(right.key()))
        }
        _ => None,
    }
}

pub(super) fn compare(
    catalog: Option<&dyn EnumLabelCatalog>,
    left: &Value,
    right: &Value,
    state: Option<&EnumComparisonState>,
) -> Result<Ordering> {
    let legacy_keys = catalog.is_none()
        && matches!((left, right), (Value::Enum(left), Value::Enum(right)) if left.label_oid().is_none() && right.label_oid().is_none());
    if state.is_none() || legacy_keys {
        if let Some(order) = native_order(left, right) {
            return Ok(order);
        }
    }
    let (Some(left), Some(right)) = (oid(catalog, left)?, oid(catalog, right)?) else {
        return Err(SQLError::Internal(
            "enum comparison lost a strict argument".into(),
        ));
    };
    compare_oids(catalog, left, right, state)
}

fn compare_oids(
    catalog: Option<&dyn EnumLabelCatalog>,
    left: u32,
    right: u32,
    state: Option<&EnumComparisonState>,
) -> Result<Ordering> {
    // Equal and even OIDs take PostgreSQL's catalog-free paths, including
    // retained values whose output would report an invalid label identity.
    if left == right || (left & 1 == 0 && right & 1 == 0) {
        return Ok(left.cmp(&right));
    }
    let left_entry = catalog
        .map(|catalog| catalog.enum_label_position(left))
        .transpose()?
        .flatten();
    let type_oid = if let Some(oid) = state.and_then(EnumComparisonState::cached_type) {
        oid
    } else {
        let (oid, _) = left_entry.ok_or_else(|| SQLError::Routine {
            sqlstate: "22P03".into(),
            message: format!("invalid internal value for enum: {left}"),
        })?;
        state.map_or(oid, |state| state.remember_type(oid))
    };
    let Some((_, left_position)) = left_entry.filter(|(oid, _)| *oid == type_oid) else {
        return Err(missing_cached_label(catalog, type_oid, left)?);
    };
    let Some((_, right_position)) = catalog
        .map(|catalog| catalog.enum_label_position(right))
        .transpose()?
        .flatten()
        .filter(|(right_type, _)| *right_type == type_oid)
    else {
        return Err(missing_cached_label(catalog, type_oid, right)?);
    };
    Ok(left_position.cmp(&right_position))
}

fn missing_cached_label(
    catalog: Option<&dyn EnumLabelCatalog>,
    type_oid: u32,
    label_oid: u32,
) -> Result<SQLError> {
    Ok(SQLError::Routine {
        sqlstate: "XX000".into(),
        message: format!(
            "enum value {label_oid} not found in cache for enum {}",
            type_name(catalog, type_oid)?
        ),
    })
}
