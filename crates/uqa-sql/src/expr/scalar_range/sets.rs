//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    CanonicalMultirange, CanonicalRange, Produced, ProductionControl, RangeSubtype, Result,
};

// Borrow member slices directly from their admitted parser owners; accessors and comparisons do not clone containers or numeric bounds.
pub(super) enum RangeSet {
    Single(Produced<CanonicalRange>),
    Multiple(Produced<CanonicalMultirange>),
}

impl RangeSet {
    pub(super) fn read(
        value: &super::Value,
        subtype: RangeSubtype,
        multirange: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        Ok(if multirange {
            Self::Multiple(Self::multiple(value, subtype, control)?)
        } else {
            Self::Single(Self::single(value, subtype, control)?)
        })
    }

    pub(super) fn single(
        value: &super::Value,
        subtype: RangeSubtype,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CanonicalRange>> {
        match super::super::range::RangeComparisonValue::from_type_with_control(
            value,
            &crate::ColumnType::Range(subtype),
            None,
            control,
        )? {
            Some(super::super::range::RangeComparisonValue::Range(range)) => Ok(range),
            _ => Err(super::SQLError::TypeMismatch(
                "range value is required".into(),
            )),
        }
    }

    pub(super) fn multiple(
        value: &super::Value,
        subtype: RangeSubtype,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CanonicalMultirange>> {
        match super::super::range::RangeComparisonValue::from_type_with_control(
            value,
            &crate::ColumnType::Multirange(subtype),
            None,
            control,
        )? {
            Some(super::super::range::RangeComparisonValue::Multirange(ranges)) => Ok(ranges),
            _ => Err(super::SQLError::TypeMismatch(
                "multirange value is required".into(),
            )),
        }
    }

    pub(super) fn read_auto(
        value: &super::Value,
        subtype: RangeSubtype,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        let multirange = match value {
            super::Value::Datum(datum) => matches!(
                crate::catalog::type_metadata::builtin_scalar_type(datum.type_oid()),
                Some(crate::ColumnType::Multirange(_))
            ),
            _ => super::range_text(value)?.trim_start().starts_with('{'),
        };
        Self::read(value, subtype, multirange, control)
    }

    pub(super) fn ranges(&self) -> &[CanonicalRange] {
        match self {
            Self::Single(range) => std::slice::from_ref(&**range),
            Self::Multiple(ranges) => ranges.ranges(),
        }
    }

    pub(super) fn overlaps(&self, other: &Self, control: &ProductionControl<'_>) -> Result<bool> {
        control.check()?;
        for left in self.ranges() {
            control.check()?;
            for right in other.ranges() {
                if left.overlaps_with_control(right, control)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(super) fn contains(&self, other: &Self, control: &ProductionControl<'_>) -> Result<bool> {
        control.check()?;
        for right in other.ranges() {
            control.check()?;
            if right.is_empty() {
                continue;
            }
            let mut contains = false;
            for left in self.ranges() {
                if left.contains_range_with_control(right, control)? {
                    contains = true;
                    break;
                }
            }
            if !contains {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn adjacent(&self, other: &Self, control: &ProductionControl<'_>) -> Result<bool> {
        if self.overlaps(other, control)? {
            return Ok(false);
        }
        for left in self.ranges() {
            control.check()?;
            for right in other.ranges() {
                if left.adjacent_with_control(right, control)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}
