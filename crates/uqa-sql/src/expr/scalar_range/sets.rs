//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    parse_multirange_with_control, parse_range_with_control, CanonicalMultirange, CanonicalRange,
    Produced, ProductionControl, RangeSubtype, Result,
};

// Borrow member slices directly from their admitted parser owners; accessors and comparisons do not clone containers or numeric bounds.
pub(super) enum RangeSet {
    Single(Produced<CanonicalRange>),
    Multiple(Produced<CanonicalMultirange>),
}

impl RangeSet {
    pub(super) fn parse(
        text: &str,
        subtype: RangeSubtype,
        multirange: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        Ok(if multirange {
            Self::Multiple(parse_multirange_with_control(text, subtype, control)?)
        } else {
            Self::Single(parse_range_with_control(text, subtype, control)?)
        })
    }

    pub(super) fn parse_auto(
        text: &str,
        subtype: RangeSubtype,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        Self::parse(text, subtype, text.trim_start().starts_with('{'), control)
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
