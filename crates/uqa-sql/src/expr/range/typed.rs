//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected SQL identities give text-backed range fields their bound semantics.

use super::{
    parse_multirange_with_control, parse_range_with_control, CanonicalMultirange, CanonicalRange,
    Ordering, Produced, ProductionControl, Result, SQLError, Value,
};
use crate::{expr::SQLValueCatalog, ColumnType};

/// Admitted range bounds shared by comparison and exact SQL equality-key consumers. Parsing and normalization retain the caller's reservations until this value is released.
pub enum RangeComparisonValue {
    Range(Produced<CanonicalRange>),
    Multirange(Produced<CanonicalMultirange>),
}

impl RangeComparisonValue {
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
            return Self::from_type(value, ty, control);
        }
        let ty = catalog
            .map(|catalog| catalog.value_type_by_oid(oid))
            .transpose()?
            .flatten();
        match ty {
            Some(ty) => Self::from_type(value, &ty, control),
            None => Ok(None),
        }
    }

    fn from_type(
        value: &Value,
        ty: &ColumnType,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>> {
        control.check()?;
        let (subtype, multirange) = match ty {
            ColumnType::Range(subtype) => (*subtype, false),
            ColumnType::Multirange(subtype) => (*subtype, true),
            ColumnType::Domain { base, .. } => return Self::from_type(value, base, control),
            _ => return Ok(None),
        };
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
        match (self, other) {
            (Self::Range(left), Self::Range(right)) => left.cmp_with_control(right, control),
            (Self::Multirange(left), Self::Multirange(right)) => {
                left.cmp_with_control(right, control)
            }
            _ => Err(SQLError::Internal("range types do not match".into())),
        }
    }
}
