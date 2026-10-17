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

mod fields;
pub use fields::visit_record_image_fields_with_catalog;

/// Compare admitted records using `PostgreSQL`'s image support function. Reached field identities and arity share ordinary record comparison's lazy checks; field images use the existing controlled SQL Datum codec.
pub fn compare_record_images_with_catalog(
    left: &Value,
    right: &Value,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Ordering, SQLError> {
    control.check()?;
    let decoded_left = fields::read(left, catalog, control)?;
    let decoded_right = fields::read(right, catalog, control)?;
    super::records::compare(
        decoded_left.as_deref().unwrap_or(left),
        decoded_right.as_deref().unwrap_or(right),
        catalog,
        control,
        None,
        Ordering::Equal,
        |left, right, oid, _collation| {
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
    fields::with_type(oid, catalog, control, |ty| {
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
    })
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
