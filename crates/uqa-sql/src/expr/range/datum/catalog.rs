//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical range identities select endpoint metadata from the pinned statement generation.

use super::{corrupt, word, ColumnType, RangeSubtype, SQLError};
use crate::{catalog::type_metadata, expr::SQLValueCatalog};
use std::sync::Arc;

pub(super) enum PhysicalRangeType {
    Native(RangeSubtype),
    Declared(Arc<crate::expr::range::RangeValueDescriptor>),
}

pub(super) fn is_multirange(ty: &ColumnType) -> Result<bool, SQLError> {
    match ty {
        ColumnType::Range(_) | ColumnType::DeclaredRange(_) => Ok(false),
        ColumnType::Multirange(_) | ColumnType::DeclaredMultirange(_) => Ok(true),
        _ => Err(corrupt("non-range payload type")),
    }
}

pub(super) fn resolve(
    bytes: &[u8],
    declared: &ColumnType,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<PhysicalRangeType, SQLError> {
    let multirange = is_multirange(declared)?;
    resolve_class(bytes, multirange, catalog)
}

pub(super) fn resolve_class(
    bytes: &[u8],
    multirange: bool,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<PhysicalRangeType, SQLError> {
    let oid = word(bytes)?;
    let actual = type_metadata::builtin_scalar_type(oid);
    match actual {
        Some(ColumnType::Range(subtype)) if !multirange => {
            return Ok(PhysicalRangeType::Native(*subtype))
        }
        Some(ColumnType::Multirange(subtype)) if multirange => {
            return Ok(PhysicalRangeType::Native(*subtype))
        }
        _ => {}
    }
    let descriptor = if actual.is_none() {
        catalog
            .map(|catalog| catalog.value_range_type(oid))
            .transpose()?
            .flatten()
    } else {
        None
    };
    if let Some(descriptor) = descriptor.as_ref() {
        if descriptor.range_oid == 0
            || descriptor.multirange_oid == 0
            || descriptor.range_oid == descriptor.multirange_oid
        {
            return Err(corrupt("inconsistent range type descriptor"));
        }
        let expected = if multirange {
            descriptor.multirange_oid
        } else {
            descriptor.range_oid
        };
        if expected == oid {
            return Ok(PhysicalRangeType::Declared(Arc::clone(descriptor)));
        }
        if oid != descriptor.range_oid && oid != descriptor.multirange_oid {
            return Err(corrupt("inconsistent range type descriptor"));
        }
    }
    let exists = actual.is_some()
        || descriptor.is_some()
        || type_metadata::builtin_array_element(oid).is_some()
        || type_metadata::routine_pseudo_type_name(i64::from(oid)).is_some()
        || catalog
            .map(|catalog| catalog.value_type_exists(oid))
            .transpose()?
            .unwrap_or(false);
    if !exists {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type with OID {oid} does not exist"),
        });
    }
    Err(SQLError::Routine {
        sqlstate: "XX000".into(),
        message: format!(
            "type {oid} is not a {} type",
            if multirange { "multirange" } else { "range" }
        ),
    })
}
