//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected SQL identities preserve admitted range fields through text and physical readers.

use super::{
    parse_multirange_with_control, parse_range_with_control, CanonicalMultirange, CanonicalRange,
    Ordering, Produced, ProductionControl, Result, SQLError, Value,
};
use crate::{expr::SQLValueCatalog, ColumnType};

/// Admitted range bounds shared by comparison and exact SQL equality-key consumers. Parsing and normalization retain the caller's reservations until this value is released.
pub enum RangeComparisonValue {
    Range(Produced<CanonicalRange>),
    Multirange(Produced<CanonicalMultirange>),
    DeclaredRange(Produced<CanonicalRange<u32>>),
    DeclaredMultirange(Produced<CanonicalMultirange<u32>>),
}

impl RangeComparisonValue {
    /// Declared ranges carry an unambiguous physical SQL identity. Native text carriers still require their selected field type, so ordinary text does not acquire range comparison or key semantics from another operand.
    pub fn declared_physical_oid(
        value: &Value,
        catalog: Option<&dyn SQLValueCatalog>,
    ) -> Result<Option<u32>> {
        let Value::Datum(datum) = value else {
            return Ok(None);
        };
        let oid = datum.type_oid();
        if crate::catalog::type_metadata::builtin_scalar_type(oid).is_some() {
            return Ok(None);
        }
        Ok(catalog
            .map(|catalog| catalog.value_range_type(oid))
            .transpose()?
            .flatten()
            .map(|_| oid))
    }

    /// Resolve an already selected field type through the existing builtin or statement catalog. Text and unrelated SQL types keep their original interpretation.
    pub fn from_declared_with_control(
        value: &Value,
        oid: u32,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>> {
        control.check()?;
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        if let Some(ty) = crate::catalog::type_metadata::builtin_scalar_type(oid) {
            return Self::from_type(value, ty, catalog, control);
        }
        let ty = catalog
            .map(|catalog| catalog.value_type_by_oid(oid))
            .transpose()?
            .flatten()
            .map(|ty| ty.retain_external_with_control(control))
            .transpose()?;
        match ty {
            Some(ty) => Self::from_type(value, &ty, catalog, control),
            None => Ok(None),
        }
    }

    /// Observe an admitted value under an already selected type without resolving its identity again. Unrelated types and SQL NULL retain their ordinary consumer semantics.
    pub fn from_type_with_control(
        value: &Value,
        ty: &ColumnType,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>> {
        control.check()?;
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        Self::from_type(value, ty, catalog, control)
    }

    fn from_type(
        value: &Value,
        ty: &ColumnType,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>> {
        control.check()?;
        if matches!(
            ty,
            ColumnType::DeclaredRange(_) | ColumnType::DeclaredMultirange(_)
        ) {
            return match value {
                Value::Datum(datum) => {
                    super::super::datums::range_comparison(datum, ty, catalog, control).map(Some)
                }
                _ => Err(SQLError::Internal(
                    "declared range comparison requires admitted physical bounds".into(),
                )),
            };
        }
        let (subtype, multirange) = match ty {
            ColumnType::Range(subtype) => (*subtype, false),
            ColumnType::Multirange(subtype) => (*subtype, true),
            ColumnType::Domain { base, .. } => {
                return Self::from_type(value, base, catalog, control)
            }
            _ => return Ok(None),
        };
        if let Value::Datum(datum) = value {
            return super::super::datums::range_comparison(datum, ty, catalog, control).map(Some);
        }
        let Value::Str(text) = value else {
            return Err(SQLError::Internal(
                "selected range comparison requires an admitted range value".into(),
            ));
        };
        Ok(Some(if multirange {
            Self::Multirange(parse_multirange_with_control(text, subtype, control)?)
        } else {
            Self::Range(parse_range_with_control(text, subtype, control)?)
        }))
    }

    /// Compare values with the same selected range identity; NULL placement remains with the containing SQL operation.
    pub fn cmp_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering> {
        self.cmp_with_catalog_and_control(other, None, control)
    }

    pub fn cmp_with_catalog_and_control(
        &self,
        other: &Self,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering> {
        let compare = |oid, left: &Value, right: &Value, control: &ProductionControl<'_>| {
            catalog
                .ok_or_else(|| {
                    SQLError::Internal("selected range comparison support is unavailable".into())
                })?
                .compare_range_bounds(oid, left, right, control)
        };
        match (self, other) {
            (Self::Range(left), Self::Range(right)) => left.cmp_with_control(right, control),
            (Self::Multirange(left), Self::Multirange(right)) => {
                left.cmp_with_control(right, control)
            }
            (Self::DeclaredRange(left), Self::DeclaredRange(right)) => {
                left.cmp_by_with_control(right, control, |a, b, control| {
                    compare(*left.identity(), a, b, control)
                })
            }
            (Self::DeclaredMultirange(left), Self::DeclaredMultirange(right)) => left
                .cmp_by_with_control(right, control, |a, b, control| {
                    compare(*left.identity(), a, b, control)
                }),
            _ => Err(SQLError::Routine {
                sqlstate: "XX000".into(),
                message: "range types do not match".into(),
            }),
        }
    }
}
