//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pinned type metadata for observing admitted range payloads without invoking input or canonicalization.

use crate::{catalog::type_metadata, expr::SQLValueCatalog, ColumnType, SQLError};
use uqa_core::memory::{Produced, ProductionControl};

#[derive(Debug, Clone)]
pub struct RangeValueDescriptor {
    pub range_oid: u32,
    pub multirange_oid: u32,
    pub endpoint: ColumnType,
}

impl RangeValueDescriptor {
    /// Copy only the selected metadata under the caller's allowance; no historic catalog generation or input callback is retained.
    pub fn from_type_with_control(
        ty: &ColumnType,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, SQLError> {
        control.check()?;
        let selected;
        let (range_oid, multirange_oid, endpoint) = match ty {
            ColumnType::Range(subtype) | ColumnType::Multirange(subtype) => {
                selected = Self {
                    range_oid: type_metadata::pg_type_oid(&ColumnType::Range(*subtype)) as u32,
                    multirange_oid: type_metadata::pg_type_oid(&ColumnType::Multirange(*subtype))
                        as u32,
                    endpoint: subtype.scalar_type(),
                };
                (
                    selected.range_oid,
                    selected.multirange_oid,
                    &selected.endpoint,
                )
            }
            ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) => {
                let descriptor = catalog
                    .map(|catalog| catalog.value_range_type(reference.oid))
                    .transpose()?
                    .flatten()
                    .ok_or_else(|| missing(reference.oid))?;
                let expected = if matches!(ty, ColumnType::DeclaredRange(_)) {
                    descriptor.range_oid
                } else {
                    descriptor.multirange_oid
                };
                if expected != reference.oid {
                    return Err(SQLError::Internal(
                        "inconsistent range type descriptor".into(),
                    ));
                }
                let (endpoint, memory) = descriptor
                    .endpoint
                    .clone_with_control(control)?
                    .into_parts();
                return Ok(Some(control.finish(
                    Self {
                        range_oid: descriptor.range_oid,
                        multirange_oid: descriptor.multirange_oid,
                        endpoint,
                    },
                    memory,
                )?));
            }
            _ => return Ok(None),
        };
        let (endpoint, memory) = endpoint.clone_with_control(control)?.into_parts();
        Ok(Some(control.finish(
            Self {
                range_oid,
                multirange_oid,
                endpoint,
            },
            memory,
        )?))
    }

    /// Resolve the associated range's live identity for a multirange descriptor.
    pub fn range_type_with_control(
        &self,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, SQLError> {
        control.check()?;
        if let Some(ty) = type_metadata::builtin_scalar_type(self.range_oid) {
            return Ok(ty.clone_with_control(control)?);
        }
        let ty = catalog
            .map(|catalog| catalog.value_type_by_oid_with_control(self.range_oid, control))
            .transpose()?
            .flatten()
            .ok_or_else(|| missing(self.range_oid))?;
        if !matches!(*ty, ColumnType::DeclaredRange(_)) {
            return Err(SQLError::Internal(
                "inconsistent associated range type".into(),
            ));
        }
        Ok(ty)
    }
}

fn missing(oid: u32) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("type with OID {oid} does not exist"),
    }
}
