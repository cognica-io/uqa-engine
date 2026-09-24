//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range relationships compare admitted numeric bounds; covers copy only their selected endpoints.

use super::{
    production::compare_values, CanonicalMultirange, CanonicalRange, Ordering, Produced,
    ProductionControl, RangeSubtype, Result, Value,
};

impl CanonicalRange {
    pub(in crate::expr) fn overlaps_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<bool> {
        control.check()?;
        Ok(self.subtype == other.subtype
            && !self.empty
            && !other.empty
            && !upper_before_lower(self, other, control)?
            && !upper_before_lower(other, self, control)?)
    }

    pub(in crate::expr) fn adjacent_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<bool> {
        control.check()?;
        if self.subtype != other.subtype
            || self.empty
            || other.empty
            || self.overlaps_with_control(other, control)?
        {
            return Ok(false);
        }
        Ok(touching(
            self.upper(),
            self.upper_inclusive,
            other.lower(),
            other.lower_inclusive,
            control,
        )? || touching(
            other.upper(),
            other.upper_inclusive,
            self.lower(),
            self.lower_inclusive,
            control,
        )?)
    }

    pub(in crate::expr) fn contains_range_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<bool> {
        control.check()?;
        if self.subtype != other.subtype || self.empty {
            return Ok(false);
        }
        if other.empty {
            return Ok(true);
        }
        let lower = match (self.lower(), other.lower()) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(left), Some(right)) => match compare_values(left, right, control)? {
                Ordering::Less => true,
                Ordering::Greater => false,
                Ordering::Equal => self.lower_inclusive || !other.lower_inclusive,
            },
        };
        if !lower {
            return Ok(false);
        }
        Ok(match (self.upper(), other.upper()) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(left), Some(right)) => match compare_values(left, right, control)? {
                Ordering::Greater => true,
                Ordering::Less => false,
                Ordering::Equal => self.upper_inclusive || !other.upper_inclusive,
            },
        })
    }

    pub(in crate::expr) fn merge_cover_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        cover(
            [self, other],
            if self.empty {
                other.subtype
            } else {
                self.subtype
            },
            control,
        )
    }
}

impl CanonicalMultirange {
    pub(in crate::expr) fn merge_cover_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CanonicalRange>> {
        cover(self.ranges.iter(), self.subtype, control)
    }
}

fn upper_before_lower(
    left: &CanonicalRange,
    right: &CanonicalRange,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    Ok(match (left.upper(), right.lower()) {
        (None, _) | (_, None) => false,
        (Some(upper), Some(lower)) => match compare_values(upper, lower, control)? {
            Ordering::Less => true,
            Ordering::Greater => false,
            Ordering::Equal => !(left.upper_inclusive && right.lower_inclusive),
        },
    })
}

fn touching(
    upper: Option<&Value>,
    upper_inclusive: bool,
    lower: Option<&Value>,
    lower_inclusive: bool,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    Ok(match (upper, lower) {
        (Some(upper), Some(lower)) => {
            compare_values(upper, lower, control)?.is_eq() && upper_inclusive != lower_inclusive
        }
        _ => false,
    })
}

fn cover<'a>(
    ranges: impl IntoIterator<Item = &'a CanonicalRange>,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalRange>> {
    let mut bounds: Option<BorrowedCover<'_>> = None;
    for range in ranges {
        control.check()?;
        if range.empty {
            continue;
        }
        if let Some(bounds) = &mut bounds {
            bounds.include(range, control)?;
        } else {
            bounds = Some(BorrowedCover {
                subtype: range.subtype,
                lower: range.lower(),
                upper: range.upper(),
                lower_inclusive: range.lower_inclusive,
                upper_inclusive: range.upper_inclusive,
            });
        }
    }
    let Some(bounds) = bounds else {
        return Ok(control.finish(CanonicalRange::empty(subtype), control.empty_reservation())?);
    };
    let lower = copy_bound(bounds.lower, control)?;
    let upper = copy_bound(bounds.upper, control)?;
    let (lower, lower_memory) = lower.into_parts();
    let (upper, upper_memory) = upper.into_parts();
    Ok(control.finish(
        CanonicalRange {
            subtype: bounds.subtype,
            lower,
            upper,
            lower_inclusive: bounds.lower_inclusive,
            upper_inclusive: bounds.upper_inclusive,
            empty: false,
        },
        control.combine(lower_memory, upper_memory),
    )?)
}

struct BorrowedCover<'a> {
    subtype: RangeSubtype,
    lower: Option<&'a Value>,
    upper: Option<&'a Value>,
    lower_inclusive: bool,
    upper_inclusive: bool,
}

impl<'a> BorrowedCover<'a> {
    fn include(
        &mut self,
        range: &'a CanonicalRange,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        match (self.lower, range.lower()) {
            (None, _) => {}
            (_, None) => {
                self.lower = None;
                self.lower_inclusive = false;
            }
            (Some(left), Some(right)) => match compare_values(left, right, control)? {
                Ordering::Greater => {
                    self.lower = Some(right);
                    self.lower_inclusive = range.lower_inclusive;
                }
                Ordering::Equal => self.lower_inclusive |= range.lower_inclusive,
                Ordering::Less => {}
            },
        }
        match (self.upper, range.upper()) {
            (None, _) => {}
            (_, None) => {
                self.upper = None;
                self.upper_inclusive = false;
            }
            (Some(left), Some(right)) => match compare_values(left, right, control)? {
                Ordering::Less => {
                    self.upper = Some(right);
                    self.upper_inclusive = range.upper_inclusive;
                }
                Ordering::Equal => self.upper_inclusive |= range.upper_inclusive,
                Ordering::Greater => {}
            },
        }
        Ok(())
    }
}

fn copy_bound(
    value: Option<&Value>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<Value>>> {
    let Some(value) = value else {
        return Ok(control.finish(None, control.empty_reservation())?);
    };
    let (value, memory) = control.copy_value(value)?.into_parts();
    Ok(control.finish(Some(value), memory)?)
}
