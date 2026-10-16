//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected record image comparison observes admitted field storage without invoking field operators.

use std::cmp::Ordering;
use uqa_core::{memory::ProductionControl, Value};

use crate::{
    catalog::type_metadata,
    expr::{datums, SQLValueCatalog},
    SQLError,
};

/// Compare admitted records using `PostgreSQL`'s image support function. Reached field identities and arity share ordinary record comparison's lazy checks; field images use the existing controlled SQL Datum codec.
pub fn compare_record_images_with_catalog(
    left: &Value,
    right: &Value,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Ordering, SQLError> {
    control.check()?;
    let read = |value: &Value| match value {
        Value::Datum(datum) => {
            datums::read_with_value_catalog_and_control(datum, catalog, control).map(Some)
        }
        _ => Ok(None),
    };
    let decoded_left = read(left)?;
    let decoded_right = read(right)?;
    super::records::compare(
        decoded_left.as_deref().unwrap_or(left),
        decoded_right.as_deref().unwrap_or(right),
        catalog,
        control,
        None,
        Ordering::Equal,
        |left, right, oid| {
            let order = match (matches!(left, Value::Null), matches!(right, Value::Null)) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => compare_field(left, right, oid, catalog, control)?,
            };
            Ok((!order.is_eq()).then_some(order))
        },
    )
}

fn compare_field(
    left: &Value,
    right: &Value,
    oid: Option<u32>,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Ordering, SQLError> {
    let oid = oid.ok_or_else(|| {
        SQLError::Internal("record image field has no selected SQL type identity".into())
    })?;
    let selected;
    let ty = if let Some(ty) = type_metadata::builtin_scalar_type(oid) {
        ty
    } else {
        selected = catalog
            .map(|catalog| catalog.value_type_by_oid(oid))
            .transpose()?
            .flatten()
            .map(|ty| ty.retain_external_with_control(control))
            .transpose()?;
        selected.as_deref().ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type with OID {oid} does not exist"),
        })?
    };
    let composites = super::support::ValueCompositeCatalog(catalog);
    let left = datums::encoding::encode(left, ty, Some(&composites), control)?;
    let right = datums::encoding::encode(right, ty, Some(&composites), control)?;
    if type_metadata::pg_type_by_value(ty) {
        return Ok(datum_word(&left)?.cmp(&datum_word(&right)?));
    }
    for (left, right) in left.chunks(4096).zip(right.chunks(4096)) {
        control.check()?;
        let order = left.cmp(right);
        if !order.is_eq() {
            return Ok(order);
        }
    }
    Ok(left.len().cmp(&right.len()))
}

fn datum_word(bytes: &[u8]) -> Result<u64, SQLError> {
    let mut word = [0; 8];
    word.get_mut(..bytes.len())
        .ok_or_else(datums::encoding::invalid)?
        .copy_from_slice(bytes);
    Ok(u64::from_le_bytes(word))
}

#[cfg(test)]
mod tests;
