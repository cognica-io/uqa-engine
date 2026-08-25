//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` built-in range and multirange text carriers.

use std::cmp::Ordering;

use uqa_core::{DecimalValue, TemporalValue, Value};

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
        self.subtype == other.subtype
            && !self.empty
            && !other.empty
            && !upper_before_lower(self, other)
            && !upper_before_lower(other, self)
    }

    #[must_use]
    pub fn adjacent(&self, other: &Self) -> bool {
        if self.subtype != other.subtype || self.empty || other.empty || self.overlaps(other) {
            return false;
        }
        touching_bounds(
            self.upper(),
            self.upper_inclusive,
            other.lower(),
            other.lower_inclusive,
        ) || touching_bounds(
            other.upper(),
            other.upper_inclusive,
            self.lower(),
            self.lower_inclusive,
        )
    }

    #[must_use]
    pub fn contains_range(&self, other: &Self) -> bool {
        if self.subtype != other.subtype || self.empty {
            return false;
        }
        if other.empty {
            return true;
        }
        lower_contains(self, other) && upper_contains(self, other)
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

    fn merge(&self, other: &Self) -> Self {
        debug_assert!(self.overlaps(other) || self.adjacent(other));
        let (lower, lower_inclusive) = minimum_lower(self, other);
        let (upper, upper_inclusive) = maximum_upper(self, other);
        Self {
            subtype: self.subtype,
            lower,
            upper,
            lower_inclusive,
            upper_inclusive,
            empty: false,
        }
    }

    /// Smallest range containing both operands. Unlike union, `PostgreSQL`'s
    /// `range_merge` also spans a gap between disjoint ranges.
    #[must_use]
    pub fn merge_cover(&self, other: &Self) -> Self {
        if self.empty {
            return other.clone();
        }
        if other.empty {
            return self.clone();
        }
        let (lower, lower_inclusive) = minimum_lower(self, other);
        let (upper, upper_inclusive) = maximum_upper(self, other);
        Self {
            subtype: self.subtype,
            lower,
            upper,
            lower_inclusive,
            upper_inclusive,
            empty: false,
        }
    }

    #[must_use]
    pub fn to_text(&self) -> String {
        if self.empty {
            return "empty".into();
        }
        let mut text = String::new();
        text.push(if self.lower_inclusive { '[' } else { '(' });
        if let Some(lower) = &self.lower {
            text.push_str(&format_bound(lower));
        }
        text.push(',');
        if let Some(upper) = &self.upper {
            text.push_str(&format_bound(upper));
        }
        text.push(if self.upper_inclusive { ']' } else { ')' });
        text
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
        self.ranges
            .iter()
            .cloned()
            .reduce(|left, right| left.merge_cover(&right))
            .unwrap_or_else(|| CanonicalRange::empty(self.subtype))
    }

    #[must_use]
    pub fn to_text(&self) -> String {
        format!(
            "{{{}}}",
            self.ranges
                .iter()
                .map(CanonicalRange::to_text)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

pub fn parse_range(text: &str, subtype: RangeSubtype) -> Result<CanonicalRange> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("empty") {
        return Ok(CanonicalRange {
            subtype,
            lower: None,
            upper: None,
            lower_inclusive: false,
            upper_inclusive: false,
            empty: true,
        });
    }
    let mut chars = text.chars();
    let opening = chars.next().ok_or_else(|| invalid_range(text, subtype))?;
    let closing = text
        .chars()
        .next_back()
        .ok_or_else(|| invalid_range(text, subtype))?;
    if !matches!(opening, '[' | '(') || !matches!(closing, ']' | ')') || text.len() < 2 {
        return Err(invalid_range(text, subtype));
    }
    let body = &text[opening.len_utf8()..text.len() - closing.len_utf8()];
    let (lower_text, upper_text) =
        split_range_bounds(body).ok_or_else(|| invalid_range(text, subtype))?;
    let mut lower = parse_bound(lower_text, subtype, text)?;
    let mut upper = parse_bound(upper_text, subtype, text)?;
    let mut lower_inclusive = opening == '[' && lower.is_some();
    let mut upper_inclusive = closing == ']' && upper.is_some();
    if is_discrete(subtype) {
        if !lower_inclusive {
            if let Some(value) = lower.as_ref() {
                lower = Some(increment_discrete(value, subtype)?);
                lower_inclusive = true;
            }
        }
        if upper_inclusive {
            if let Some(value) = upper.as_ref() {
                upper = Some(increment_discrete(value, subtype)?);
                upper_inclusive = false;
            }
        }
    }
    let empty = match (&lower, &upper) {
        (Some(lower), Some(upper)) => match lower.cmp(upper) {
            Ordering::Greater => true,
            Ordering::Equal => !(lower_inclusive && upper_inclusive),
            Ordering::Less => false,
        },
        _ => false,
    };
    if empty {
        return parse_range("empty", subtype);
    }
    Ok(CanonicalRange {
        subtype,
        lower,
        upper,
        lower_inclusive,
        upper_inclusive,
        empty: false,
    })
}

pub fn parse_multirange(text: &str, subtype: RangeSubtype) -> Result<CanonicalMultirange> {
    let text = text.trim();
    if !text.starts_with('{') || !text.ends_with('}') {
        return Err(invalid_multirange(text, subtype));
    }
    let body = &text[1..text.len() - 1];
    let mut ranges = split_multirange_items(body)
        .ok_or_else(|| invalid_multirange(text, subtype))?
        .into_iter()
        .map(|item| parse_range(item, subtype))
        .collect::<Result<Vec<_>>>()?;
    ranges.retain(|range| !range.empty);
    ranges.sort_by(compare_lower_bounds);
    let mut normalized: Vec<CanonicalRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(previous) = normalized.last_mut() {
            if previous.overlaps(&range) || previous.adjacent(&range) {
                *previous = previous.merge(&range);
                continue;
            }
        }
        normalized.push(range);
    }
    Ok(CanonicalMultirange {
        subtype,
        ranges: normalized,
    })
}

pub fn multirange_from_ranges(
    subtype: RangeSubtype,
    ranges: impl IntoIterator<Item = CanonicalRange>,
) -> CanonicalMultirange {
    let mut ranges = ranges
        .into_iter()
        .filter(|range| !range.empty)
        .collect::<Vec<_>>();
    ranges.sort_by(compare_lower_bounds);
    let mut normalized: Vec<CanonicalRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(previous) = normalized.last_mut() {
            if previous.overlaps(&range) || previous.adjacent(&range) {
                *previous = previous.merge(&range);
                continue;
            }
        }
        normalized.push(range);
    }
    CanonicalMultirange {
        subtype,
        ranges: normalized,
    }
}

fn split_range_bounds(body: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in body.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quoted = !quoted;
            continue;
        }
        if character == ',' && !quoted {
            return Some((&body[..index], &body[index + 1..]));
        }
    }
    None
}

fn split_multirange_items(body: &str) -> Option<Vec<&str>> {
    if body.trim().is_empty() {
        return Some(Vec::new());
    }
    let mut items = Vec::new();
    let mut start = None;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in body.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        match character {
            '[' | '(' if start.is_none() => start = Some(index),
            ']' | ')' => {
                let item_start = start.take()?;
                items.push(body[item_start..=index].trim());
            }
            ',' if start.is_none() => {}
            _ => {}
        }
    }
    if quoted || escaped || start.is_some() || items.is_empty() {
        None
    } else {
        Some(items)
    }
}

fn parse_bound(raw: &str, subtype: RangeSubtype, whole: &str) -> Result<Option<Value>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let text = unquote_bound(raw).ok_or_else(|| invalid_range(whole, subtype))?;
    let value = match subtype {
        RangeSubtype::Integer => text
            .parse::<i32>()
            .map(|value| Value::Int(i64::from(value)))
            .map_err(|_| range_subtype_error(&text, "integer"))?,
        RangeSubtype::BigInteger => text
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| range_subtype_error(&text, "bigint"))?,
        RangeSubtype::Numeric => DecimalValue::parse(&text)
            .map(Value::Decimal)
            .ok_or_else(|| range_subtype_error(&text, "numeric"))?,
        RangeSubtype::Date => TemporalValue::try_parse_date(&text)
            .map(Value::Temporal)
            .map_err(|_| range_subtype_error(&text, "date"))?,
        RangeSubtype::Timestamp => TemporalValue::parse_timestamp(&text)
            .map(Value::Temporal)
            .ok_or_else(|| range_subtype_error(&text, "timestamp without time zone"))?,
        RangeSubtype::TimestampTz => TemporalValue::parse_timestamp_tz(&text)
            .map(Value::Temporal)
            .ok_or_else(|| range_subtype_error(&text, "timestamp with time zone"))?,
    };
    Ok(Some(value))
}

fn unquote_bound(raw: &str) -> Option<String> {
    if !raw.starts_with('"') {
        return (!raw.contains('"')).then(|| raw.to_string());
    }
    if raw.len() < 2 || !raw.ends_with('"') {
        return None;
    }
    let mut value = String::new();
    let mut chars = raw[1..raw.len() - 1].chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            value.push(chars.next()?);
        } else {
            value.push(character);
        }
    }
    Some(value)
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

fn upper_before_lower(left: &CanonicalRange, right: &CanonicalRange) -> bool {
    match (left.upper(), right.lower()) {
        (None, _) | (_, None) => false,
        (Some(upper), Some(lower)) => match upper.cmp(lower) {
            Ordering::Less => true,
            Ordering::Greater => false,
            Ordering::Equal => !(left.upper_inclusive && right.lower_inclusive),
        },
    }
}

fn touching_bounds(
    upper: Option<&Value>,
    upper_inclusive: bool,
    lower: Option<&Value>,
    lower_inclusive: bool,
) -> bool {
    matches!((upper, lower), (Some(upper), Some(lower)) if upper == lower)
        && upper_inclusive != lower_inclusive
}

fn lower_contains(outer: &CanonicalRange, inner: &CanonicalRange) -> bool {
    match (outer.lower(), inner.lower()) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(left), Some(right)) => match left.cmp(right) {
            Ordering::Less => true,
            Ordering::Greater => false,
            Ordering::Equal => outer.lower_inclusive || !inner.lower_inclusive,
        },
    }
}

fn upper_contains(outer: &CanonicalRange, inner: &CanonicalRange) -> bool {
    match (outer.upper(), inner.upper()) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(left), Some(right)) => match left.cmp(right) {
            Ordering::Greater => true,
            Ordering::Less => false,
            Ordering::Equal => outer.upper_inclusive || !inner.upper_inclusive,
        },
    }
}

fn minimum_lower(left: &CanonicalRange, right: &CanonicalRange) -> (Option<Value>, bool) {
    match (left.lower(), right.lower()) {
        (None, _) | (_, None) => (None, false),
        (Some(left_value), Some(right_value)) => match left_value.cmp(right_value) {
            Ordering::Less => (Some(left_value.clone()), left.lower_inclusive),
            Ordering::Greater => (Some(right_value.clone()), right.lower_inclusive),
            Ordering::Equal => (
                Some(left_value.clone()),
                left.lower_inclusive || right.lower_inclusive,
            ),
        },
    }
}

fn maximum_upper(left: &CanonicalRange, right: &CanonicalRange) -> (Option<Value>, bool) {
    match (left.upper(), right.upper()) {
        (None, _) | (_, None) => (None, false),
        (Some(left_value), Some(right_value)) => match left_value.cmp(right_value) {
            Ordering::Greater => (Some(left_value.clone()), left.upper_inclusive),
            Ordering::Less => (Some(right_value.clone()), right.upper_inclusive),
            Ordering::Equal => (
                Some(left_value.clone()),
                left.upper_inclusive || right.upper_inclusive,
            ),
        },
    }
}

fn compare_lower_bounds(left: &CanonicalRange, right: &CanonicalRange) -> Ordering {
    match (left.lower(), right.lower()) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left_value), Some(right_value)) => left_value
            .cmp(right_value)
            .then_with(|| right.lower_inclusive.cmp(&left.lower_inclusive)),
    }
}

fn format_bound(value: &Value) -> String {
    let raw = match value {
        Value::Int(value) => value.to_string(),
        Value::Decimal(value) => value.to_sql_string(),
        Value::Temporal(value) => value.to_sql_string(),
        other => super::value_to_string(other),
    };
    if raw.is_empty()
        || raw.chars().any(|character| {
            character.is_whitespace()
                || matches!(character, ',' | '[' | ']' | '(' | ')' | '"' | '\\')
        })
    {
        format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        raw
    }
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
