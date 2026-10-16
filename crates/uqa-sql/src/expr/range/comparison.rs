//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range order uses the selected subtype's bounds, independently of text spelling.

use super::{
    production::{compare_lower_bounds, compare_values},
    CanonicalMultirange, CanonicalRange, Ordering, ProductionControl, Result, SQLError,
};

impl CanonicalRange {
    /// Compare admitted ranges of one subtype in PostgreSQL B-tree order, retaining the caller's comparison workspace and cancellation boundary.
    pub fn cmp_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering> {
        control.check()?;
        if self.subtype != other.subtype {
            return Err(SQLError::Internal("range types do not match".into()));
        }
        match (self.empty, other.empty) {
            (true, true) => return Ok(Ordering::Equal),
            (true, false) => return Ok(Ordering::Less),
            (false, true) => return Ok(Ordering::Greater),
            (false, false) => {}
        }
        let lower = compare_lower_bounds(self, other, control)?;
        if !lower.is_eq() {
            return Ok(lower);
        }
        Ok(match (&self.upper, &other.upper) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) => compare_values(left, right, control)?
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
        control.check()?;
        if self.subtype != other.subtype {
            return Err(SQLError::Internal("multirange types do not match".into()));
        }
        for (left, right) in self.ranges.iter().zip(&other.ranges) {
            let order = left.cmp_with_control(right, control)?;
            if !order.is_eq() {
                return Ok(order);
            }
        }
        Ok(self.ranges.len().cmp(&other.ranges.len()))
    }
}

#[cfg(test)]
mod tests;
