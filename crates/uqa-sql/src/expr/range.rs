//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` built-in range and multirange text carriers.

use std::cmp::Ordering;

use uqa_core::{
    memory::{Produced, ProductionControl},
    TemporalValue, Value,
};

mod production;
mod relationships;
pub(super) use production::{
    canonical_multirange_text_with_control, canonical_range_as_multirange_text_with_control,
    canonical_range_text_with_control, multirange_from_produced_ranges,
    parse_multirange_with_control, parse_range_with_control,
};

use crate::ast::RangeSubtype;
use crate::error::Result;
use crate::SQLError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalRange {
    subtype: RangeSubtype,
    lower: Option<Value>,
    upper: Option<Value>,
    lower_inclusive: bool,
    upper_inclusive: bool,
    empty: bool,
}

impl CanonicalRange {
    fn empty(subtype: RangeSubtype) -> Self {
        Self {
            subtype,
            lower: None,
            upper: None,
            lower_inclusive: false,
            upper_inclusive: false,
            empty: true,
        }
    }

    #[must_use]
    pub const fn subtype(&self) -> RangeSubtype {
        self.subtype
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.empty
    }

    #[must_use]
    pub const fn lower_inclusive(&self) -> bool {
        self.lower_inclusive
    }

    #[must_use]
    pub const fn upper_inclusive(&self) -> bool {
        self.upper_inclusive
    }

    #[must_use]
    pub fn lower(&self) -> Option<&Value> {
        self.lower.as_ref()
    }

    #[must_use]
    pub fn upper(&self) -> Option<&Value> {
        self.upper.as_ref()
    }

    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.overlaps_with_control(other, &ProductionControl::uncontrolled())
            .expect("ordinary range overlap")
    }

    #[must_use]
    pub fn adjacent(&self, other: &Self) -> bool {
        self.adjacent_with_control(other, &ProductionControl::uncontrolled())
            .expect("ordinary range adjacency")
    }

    #[must_use]
    pub fn contains_range(&self, other: &Self) -> bool {
        self.contains_range_with_control(other, &ProductionControl::uncontrolled())
            .expect("ordinary range containment")
    }

    #[must_use]
    pub fn contains_value(&self, value: &Value) -> bool {
        if self.empty {
            return false;
        }
        let lower = self
            .lower
            .as_ref()
            .is_none_or(|lower| match value.cmp(lower) {
                Ordering::Greater => true,
                Ordering::Equal => self.lower_inclusive,
                Ordering::Less => false,
            });
        let upper = self
            .upper
            .as_ref()
            .is_none_or(|upper| match value.cmp(upper) {
                Ordering::Less => true,
                Ordering::Equal => self.upper_inclusive,
                Ordering::Greater => false,
            });
        lower && upper
    }

    /// Smallest range containing both operands. Unlike union, `PostgreSQL`'s
    /// `range_merge` also spans a gap between disjoint ranges.
    #[must_use]
    pub fn merge_cover(&self, other: &Self) -> Self {
        self.merge_cover_with_control(other, &ProductionControl::uncontrolled())
            .expect("ordinary range cover")
            .into_uncontrolled()
            .expect("ordinary range")
    }

    pub(super) fn to_text_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>> {
        production::range_text(self, control)
    }

    #[must_use]
    pub fn to_text(&self) -> String {
        production::range_text(self, &ProductionControl::uncontrolled())
            .expect("ordinary range formatting")
            .into_uncontrolled()
            .expect("ordinary range text")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalMultirange {
    subtype: RangeSubtype,
    ranges: Vec<CanonicalRange>,
}

impl CanonicalMultirange {
    #[must_use]
    pub fn ranges(&self) -> &[CanonicalRange] {
        &self.ranges
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    #[must_use]
    pub fn contains_range(&self, range: &CanonicalRange) -> bool {
        range.subtype == self.subtype
            && (range.empty || self.ranges.iter().any(|item| item.contains_range(range)))
    }

    #[must_use]
    pub fn contains_multirange(&self, other: &Self) -> bool {
        self.subtype == other.subtype && other.ranges.iter().all(|range| self.contains_range(range))
    }

    #[must_use]
    pub fn overlaps_range(&self, range: &CanonicalRange) -> bool {
        range.subtype == self.subtype && self.ranges.iter().any(|item| item.overlaps(range))
    }

    #[must_use]
    pub fn overlaps_multirange(&self, other: &Self) -> bool {
        self.subtype == other.subtype
            && self
                .ranges
                .iter()
                .any(|left| other.ranges.iter().any(|right| left.overlaps(right)))
    }

    #[must_use]
    pub fn merge_cover(&self) -> CanonicalRange {
        self.merge_cover_with_control(&ProductionControl::uncontrolled())
            .expect("ordinary multirange cover")
            .into_uncontrolled()
            .expect("ordinary range")
    }

    pub(super) fn to_text_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>> {
        production::multirange_text(self, control)
    }

    #[must_use]
    pub fn to_text(&self) -> String {
        production::multirange_text(self, &ProductionControl::uncontrolled())
            .expect("ordinary multirange formatting")
            .into_uncontrolled()
            .expect("ordinary multirange text")
    }
}

pub fn parse_range(text: &str, subtype: RangeSubtype) -> Result<CanonicalRange> {
    Ok(
        production::parse_range_with_control(text, subtype, &ProductionControl::uncontrolled())?
            .into_uncontrolled()
            .expect("ordinary range"),
    )
}

pub fn parse_multirange(text: &str, subtype: RangeSubtype) -> Result<CanonicalMultirange> {
    Ok(production::parse_multirange_with_control(
        text,
        subtype,
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary multirange"))
}

pub fn multirange_from_ranges(
    subtype: RangeSubtype,
    ranges: impl IntoIterator<Item = CanonicalRange>,
) -> CanonicalMultirange {
    production::normalize_ranges(ranges, subtype, &ProductionControl::uncontrolled(), None)
        .expect("ordinary multirange normalization")
        .into_uncontrolled()
        .expect("ordinary multirange")
}

fn increment_discrete(value: &Value, subtype: RangeSubtype) -> Result<Value> {
    match (subtype, value) {
        (RangeSubtype::Integer, Value::Int(value)) => i32::try_from(*value)
            .ok()
            .and_then(|value| value.checked_add(1))
            .map(|value| Value::Int(i64::from(value)))
            .ok_or_else(|| range_overflow("integer")),
        (RangeSubtype::BigInteger, Value::Int(value)) => value
            .checked_add(1)
            .map(Value::Int)
            .ok_or_else(|| range_overflow("bigint")),
        (RangeSubtype::Date, Value::Temporal(TemporalValue::Date { days })) => days
            .checked_add(1)
            .map(|days| Value::Temporal(TemporalValue::Date { days }))
            .ok_or_else(|| range_overflow("date")),
        _ => Err(SQLError::Internal(format!(
            "range subtype {subtype:?} received incompatible bound {value:?}"
        ))),
    }
}

fn is_discrete(subtype: RangeSubtype) -> bool {
    matches!(
        subtype,
        RangeSubtype::Integer | RangeSubtype::BigInteger | RangeSubtype::Date
    )
}

fn invalid_range(text: &str, subtype: RangeSubtype) -> SQLError {
    SQLError::Routine {
        sqlstate: "22P02".into(),
        message: format!(
            "malformed range literal: \"{text}\" for type {}",
            subtype.range_name()
        ),
    }
}

fn invalid_multirange(text: &str, subtype: RangeSubtype) -> SQLError {
    SQLError::Routine {
        sqlstate: "22P02".into(),
        message: format!(
            "malformed multirange literal: \"{text}\" for type {}",
            subtype.multirange_name()
        ),
    }
}

fn range_subtype_error(text: &str, type_name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22P02".into(),
        message: format!("invalid input syntax for type {type_name}: \"{text}\""),
    }
}

fn range_overflow(type_name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22003".into(),
        message: format!("{type_name} out of range"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discrete_ranges_canonicalize_to_inclusive_exclusive_bounds() {
        assert_eq!(
            parse_range("(1,4]", RangeSubtype::Integer)
                .unwrap()
                .to_text(),
            "[2,5)"
        );
        assert_eq!(
            parse_range("[2024-01-01,2024-01-02]", RangeSubtype::Date)
                .unwrap()
                .to_text(),
            "[2024-01-01,2024-01-03)"
        );
    }

    #[test]
    fn multiranges_merge_overlapping_and_adjacent_members() {
        assert_eq!(
            parse_multirange("{[10,12),[1,3),[3,5)}", RangeSubtype::Integer)
                .unwrap()
                .to_text(),
            "{[1,5),[10,12)}"
        );
    }

    #[test]
    fn range_relationships_cover_temporal_constraint_checks() {
        let left = parse_range("[1,3)", RangeSubtype::Integer).unwrap();
        let right = parse_range("[3,5)", RangeSubtype::Integer).unwrap();
        let coverage = multirange_from_ranges(RangeSubtype::Integer, [left, right]);
        let child = parse_range("[2,4)", RangeSubtype::Integer).unwrap();
        assert!(coverage.contains_range(&child));
    }
}
