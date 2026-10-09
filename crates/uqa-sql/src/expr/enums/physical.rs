//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical enum operations read admitted OIDs without repeating enum input or output.

use std::cmp::Ordering;

use uqa_core::Value;

use super::{enum_label_oid, type_name, EnumLabelCatalog};
use crate::error::{Result, SQLError};

#[cfg(test)]
mod tests;

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
) -> Result<Ordering> {
    if let Some(order) = native_order(left, right) {
        return Ok(order);
    }
    let (Some(left), Some(right)) = (oid(catalog, left)?, oid(catalog, right)?) else {
        return Err(SQLError::Internal(
            "enum comparison lost a strict argument".into(),
        ));
    };
    compare_oids(catalog, left, right)
}

fn compare_oids(catalog: Option<&dyn EnumLabelCatalog>, left: u32, right: u32) -> Result<Ordering> {
    // Equal and even OIDs take PostgreSQL's catalog-free paths, including
    // retained values whose output would report an invalid label identity.
    if left == right || (left & 1 == 0 && right & 1 == 0) {
        return Ok(left.cmp(&right));
    }
    let (type_oid, left_position) = catalog
        .map(|catalog| catalog.enum_label_position(left))
        .transpose()?
        .flatten()
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "22P03".into(),
            message: format!("invalid internal value for enum: {left}"),
        })?;
    let Some((_, right_position)) = catalog
        .map(|catalog| catalog.enum_label_position(right))
        .transpose()?
        .flatten()
        .filter(|(right_type, _)| *right_type == type_oid)
    else {
        return Err(SQLError::Routine {
            sqlstate: "XX000".into(),
            message: format!(
                "enum value {right} not found in cache for enum {}",
                type_name(catalog, type_oid)?
            ),
        });
    };
    Ok(left_position.cmp(&right_position))
}
