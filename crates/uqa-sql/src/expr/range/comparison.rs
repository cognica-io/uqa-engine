//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range order uses the selected subtype's bounds, independently of text spelling.

use super::{
    CanonicalMultirange, CanonicalRange, Ordering, ProductionControl, RangeSemantics, Result,
    SQLError, Value,
};

impl CanonicalRange {
    /// Compare admitted ranges of one subtype in `PostgreSQL` B-tree order, retaining the caller's comparison workspace and cancellation boundary.
    pub fn cmp_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering> {
        self.cmp_by_with_control(other, control, |left, right, control| {
            self.semantics().compare(left, right, control)
        })
    }
}

impl<I: PartialEq> CanonicalRange<I> {
    /// Compare one physical range family using its already selected endpoint operation. The caller retains operator-class, collation and invocation state; range identity never selects a native canonicalizer implicitly.
    pub(crate) fn cmp_by_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
        mut compare: impl FnMut(&Value, &Value, &ProductionControl<'_>) -> Result<Ordering>,
    ) -> Result<Ordering> {
        control.check()?;
        if self.subtype != other.subtype {
            return Err(SQLError::Routine {
                sqlstate: "XX000".into(),
                message: "range types do not match".into(),
            });
        }
        match (self.empty, other.empty) {
            (true, true) => return Ok(Ordering::Equal),
            (true, false) => return Ok(Ordering::Less),
            (false, true) => return Ok(Ordering::Greater),
            (false, false) => {}
        }
        let lower = compare_lower_bounds_by(self, other, control, &mut compare)?;
        if !lower.is_eq() {
            return Ok(lower);
        }
        Ok(match (&self.upper, &other.upper) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) => compare(left, right, control)?
                .then_with(|| self.upper_inclusive.cmp(&other.upper_inclusive)),
        })
    }
}

impl CanonicalMultirange {
    /// Compare normalized members in order; an equal prefix places the shorter multirange first.
    pub fn cmp_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering> {
        let semantics = RangeSemantics::native(self.subtype);
        self.cmp_by_with_control(other, control, |left, right, control| {
            semantics.compare(left, right, control)
        })
    }
}

impl<I: PartialEq> CanonicalMultirange<I> {
    pub(crate) fn cmp_by_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
        mut compare: impl FnMut(&Value, &Value, &ProductionControl<'_>) -> Result<Ordering>,
    ) -> Result<Ordering> {
        control.check()?;
        if self.subtype != other.subtype {
            return Err(SQLError::Routine {
                sqlstate: "XX000".into(),
                message: "multirange types do not match".into(),
            });
        }
        for (left, right) in self.ranges.iter().zip(&other.ranges) {
            let order = left.cmp_by_with_control(right, control, &mut compare)?;
            if !order.is_eq() {
                return Ok(order);
            }
        }
        Ok(self.ranges.len().cmp(&other.ranges.len()))
    }
}

pub(super) fn compare_lower_bounds_by<I>(
    left: &CanonicalRange<I>,
    right: &CanonicalRange<I>,
    control: &ProductionControl<'_>,
    mut compare: impl FnMut(&Value, &Value, &ProductionControl<'_>) -> Result<Ordering>,
) -> Result<Ordering> {
    control.check()?;
    Ok(match (left.lower(), right.lower()) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left_value), Some(right_value)) => compare(left_value, right_value, control)?
            .then_with(|| right.lower_inclusive.cmp(&left.lower_inclusive)),
    })
}

#[cfg(test)]
mod tests;
