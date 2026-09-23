//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range parsing, normalization and text production share admitted constructors.

use super::{
    increment_discrete, invalid_multirange, invalid_range, is_discrete, range_subtype_error,
    CanonicalMultirange, CanonicalRange, Ordering, ProductionControl, RangeSubtype, Result,
    SQLError, TemporalValue, Value,
};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionString, ProductionVec},
    ordering::sort_by_with_control,
    DecimalValue,
};

// The complete live value precedes its lease, including while endpoints move between work buffers.
struct RangeWorkspace<T> {
    value: T,
    memory: Option<MemoryReservation>,
}

impl<T> RangeWorkspace<T> {
    fn from_produced(value: Produced<T>) -> Self {
        let (value, memory) = value.into_parts();
        Self { value, memory }
    }
}

pub(in crate::expr) fn canonical_range_text_with_control(
    text: &str,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    range_text(&*parse_range_with_control(text, subtype, control)?, control)
}

pub(in crate::expr) fn canonical_multirange_text_with_control(
    text: &str,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    multirange_text(
        &*parse_multirange_with_control(text, subtype, control)?,
        control,
    )
}

pub(in crate::expr) fn canonical_range_as_multirange_text_with_control(
    text: &str,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let range = parse_range_with_control(text, subtype, control)?;
    let mut output = ProductionString::new(*control);
    output.push('{')?;
    if !range.empty {
        write_range(&range, &mut output, control)?;
    }
    output.push('}')?;
    Ok(output.finish()?)
}

pub(in crate::expr) fn parse_range_with_control(
    text: &str,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalRange>> {
    control.check()?;
    let text = text.trim();
    if text.eq_ignore_ascii_case("empty") {
        return Ok(control.finish(CanonicalRange::empty(subtype), control.empty_reservation())?);
    }
    let opening = text
        .chars()
        .next()
        .ok_or_else(|| invalid_range(text, subtype))?;
    let closing = text
        .chars()
        .next_back()
        .ok_or_else(|| invalid_range(text, subtype))?;
    if !matches!(opening, '[' | '(') || !matches!(closing, ']' | ')') || text.len() < 2 {
        return Err(invalid_range(text, subtype));
    }
    let body = &text[opening.len_utf8()..text.len() - closing.len_utf8()];
    let (lower_text, upper_text) =
        split_range_bounds(body, control)?.ok_or_else(|| invalid_range(text, subtype))?;
    let lower = parse_bound(lower_text, subtype, text, control)?;
    let upper = parse_bound(upper_text, subtype, text, control)?;
    let lower_inclusive = opening == '[' && lower.is_some();
    let upper_inclusive = closing == ']' && upper.is_some();
    let (lower, lower_memory) = lower.into_parts();
    let (upper, upper_memory) = upper.into_parts();
    let mut owned = RangeWorkspace {
        value: CanonicalRange {
            subtype,
            lower,
            upper,
            lower_inclusive,
            upper_inclusive,
            empty: false,
        },
        memory: control.combine(lower_memory, upper_memory),
    };
    let range = &mut owned.value;
    if is_discrete(subtype) {
        if !range.lower_inclusive {
            if let Some(value) = range.lower.as_ref() {
                range.lower = Some(increment_discrete(value, subtype)?);
                range.lower_inclusive = true;
            }
        }
        if range.upper_inclusive {
            if let Some(value) = range.upper.as_ref() {
                range.upper = Some(increment_discrete(value, subtype)?);
                range.upper_inclusive = false;
            }
        }
    }
    let empty = match (&range.lower, &range.upper) {
        (Some(lower), Some(upper)) => match compare_values(lower, upper, control)? {
            Ordering::Greater => true,
            Ordering::Equal => !(range.lower_inclusive && range.upper_inclusive),
            Ordering::Less => false,
        },
        _ => false,
    };
    if empty {
        owned.value = CanonicalRange::empty(subtype);
    }
    Ok(control.finish(owned.value, owned.memory)?)
}

pub(in crate::expr) fn parse_multirange_with_control(
    text: &str,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalMultirange>> {
    control.check()?;
    let text = text.trim();
    if !text.starts_with('{') || !text.ends_with('}') {
        return Err(invalid_multirange(text, subtype));
    }
    let items = split_multirange_items(&text[1..text.len() - 1], control)?
        .ok_or_else(|| invalid_multirange(text, subtype))?;
    let mut ranges = ProductionVec::new(*control);
    for item in items.iter() {
        ranges.push_produced(parse_range_with_control(item, subtype, control)?)?;
    }
    let (ranges, memory) = ranges.finish()?.into_parts();
    normalize_ranges(ranges, subtype, control, memory)
}

pub(in crate::expr) fn multirange_from_produced_ranges(
    subtype: RangeSubtype,
    ranges: Produced<Vec<CanonicalRange>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalMultirange>> {
    let (ranges, memory) = ranges.into_parts();
    normalize_ranges(ranges, subtype, control, memory)
}

pub(super) fn normalize_ranges(
    ranges: impl IntoIterator<Item = CanonicalRange>,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
    memory: Option<MemoryReservation>,
) -> Result<Produced<CanonicalMultirange>> {
    // Both input and destination values precede the input payload lease while collection can fail.
    let mut collecting = RangeWorkspace {
        value: (ranges.into_iter(), ProductionVec::new(*control)),
        memory,
    };
    let (input, ordered) = &mut collecting.value;
    for (ordinal, range) in input.enumerate() {
        control.check()?;
        if !range.empty {
            ordered
                .push_produced(control.finish((ordinal, range), control.empty_reservation())?)?;
        }
    }
    let (input, ordered) = collecting.value;
    drop(input);
    let mut ordered = RangeWorkspace::from_produced(ordered.finish()?);
    ordered.memory = control.combine(collecting.memory, ordered.memory);
    // The ordinal preserves the previous stable sort's equal-bound endpoint choice without allocating sort scratch.
    let mut poll = || control.check().map_err(SQLError::from);
    sort_by_with_control(
        &mut ordered.value,
        &mut poll,
        |(left_index, left), (right_index, right), _| {
            Ok(compare_lower_bounds(left, right, control)?
                .then_with(|| left_index.cmp(right_index)))
        },
    )?;
    let mut normalized = ProductionVec::new(*control);
    normalized.reserve(ordered.value.len())?;
    let normalized = RangeWorkspace::from_produced(normalized.finish()?);
    let mut merging = RangeWorkspace {
        value: (ordered.value.into_iter(), normalized.value),
        memory: control.combine(ordered.memory, normalized.memory),
    };
    let (input, normalized) = &mut merging.value;
    for (_, range) in input {
        control.check()?;
        if let Some(previous) = normalized.last() {
            if joins(previous, &range, control)? {
                let previous = normalized.pop().expect("previous range");
                normalized.push(merge_owned(previous, range, control)?);
                continue;
            }
        }
        // Capacity was admitted for every input member before normalization.
        normalized.push(range);
    }
    let (input, normalized) = merging.value;
    drop(input);
    Ok(control.finish(
        CanonicalMultirange {
            subtype,
            ranges: normalized,
        },
        merging.memory,
    )?)
}

fn split_range_bounds<'a>(
    body: &'a str,
    control: &ProductionControl<'_>,
) -> Result<Option<(&'a str, &'a str)>> {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in body.char_indices() {
        control.check()?;
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
            return Ok(Some((&body[..index], &body[index + 1..])));
        }
    }
    Ok(None)
}

fn split_multirange_items<'a>(
    body: &'a str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<&'a str>>>> {
    let mut items = ProductionVec::new(*control);
    if body.trim().is_empty() {
        return Ok(Some(items.finish()?));
    }
    let mut start = None;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in body.char_indices() {
        control.check()?;
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
                let Some(item_start) = start.take() else {
                    return Ok(None);
                };
                items.push_copy(body[item_start..=index].trim())?;
            }
            _ => {}
        }
    }
    if quoted || escaped || start.is_some() || items.is_empty() {
        Ok(None)
    } else {
        Ok(Some(items.finish()?))
    }
}

fn parse_bound(
    raw: &str,
    subtype: RangeSubtype,
    whole: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<Value>>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(control.finish(None, control.empty_reservation())?);
    }
    let text = unquote_bound(raw, control)?.ok_or_else(|| invalid_range(whole, subtype))?;
    let mut memory = control.empty_reservation();
    let value = match subtype {
        RangeSubtype::Integer => text
            .parse::<i32>()
            .map(|value| Value::Int(i64::from(value)))
            .map_err(|_| range_subtype_error(&text, "integer"))?,
        RangeSubtype::BigInteger => text
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| range_subtype_error(&text, "bigint"))?,
        RangeSubtype::Numeric => {
            let parsed = DecimalValue::parse_with_control(&text, control)?
                .ok_or_else(|| range_subtype_error(&text, "numeric"))?;
            let (value, owned) = parsed.into_parts();
            memory = owned;
            Value::Decimal(value)
        }
        RangeSubtype::Date => Value::Temporal(
            TemporalValue::parse_date_with_control(&text, control)?
                .ok_or_else(|| range_subtype_error(&text, "date"))?,
        ),
        RangeSubtype::Timestamp => Value::Temporal(
            TemporalValue::parse_timestamp_with_control(&text, control)?
                .ok_or_else(|| range_subtype_error(&text, "timestamp without time zone"))?,
        ),
        RangeSubtype::TimestampTz => Value::Temporal(
            TemporalValue::parse_timestamp_tz_with_control(&text, control)?
                .ok_or_else(|| range_subtype_error(&text, "timestamp with time zone"))?,
        ),
    };
    Ok(control.finish(Some(value), memory)?)
}

fn unquote_bound(raw: &str, control: &ProductionControl<'_>) -> Result<Option<Produced<String>>> {
    if !raw.starts_with('"') {
        return Ok(if raw.contains('"') {
            None
        } else {
            Some(control.copy_text(raw)?)
        });
    }
    if raw.len() < 2 || !raw.ends_with('"') {
        return Ok(None);
    }
    let mut value = ProductionString::new(*control);
    let mut chars = raw[1..raw.len() - 1].chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            let Some(character) = chars.next() else {
                return Ok(None);
            };
            value.push(character)?;
        } else {
            value.push(character)?;
        }
    }
    Ok(Some(value.finish()?))
}

pub(super) fn compare_values(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    control.check()?;
    match (left, right) {
        (Value::Decimal(left), Value::Decimal(right)) => Ok(left.cmp_with_control(right, control)?),
        // All other range subtypes have inline integer or temporal carriers.
        _ => Ok(left.cmp(right)),
    }
}

fn compare_lower_bounds(
    left: &CanonicalRange,
    right: &CanonicalRange,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    Ok(match (left.lower(), right.lower()) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left_value), Some(right_value)) => compare_values(left_value, right_value, control)?
            .then_with(|| right.lower_inclusive.cmp(&left.lower_inclusive)),
    })
}

fn joins(
    left: &CanonicalRange,
    right: &CanonicalRange,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    if left.subtype != right.subtype {
        return Ok(false);
    }
    // Members are ordered by lower bound, so overlap or adjacency is decided at this one boundary.
    Ok(match (left.upper(), right.lower()) {
        (None, _) | (_, None) => true,
        (Some(upper), Some(lower)) => match compare_values(upper, lower, control)? {
            Ordering::Less => false,
            Ordering::Greater => true,
            Ordering::Equal => left.upper_inclusive || right.lower_inclusive,
        },
    })
}

fn merge_owned(
    mut left: CanonicalRange,
    mut right: CanonicalRange,
    control: &ProductionControl<'_>,
) -> Result<CanonicalRange> {
    // The lower sort keeps the left bound; equal values retain its lexical numeric scale, as before.
    if let (Some(a), Some(b)) = (left.lower(), right.lower()) {
        if compare_values(a, b, control)? == Ordering::Equal {
            left.lower_inclusive |= right.lower_inclusive;
        }
    }
    match (left.upper(), right.upper()) {
        (None, _) => {}
        (_, None) => {
            left.upper = None;
            left.upper_inclusive = false;
        }
        (Some(a), Some(b)) => match compare_values(a, b, control)? {
            Ordering::Less => {
                left.upper = right.upper.take();
                left.upper_inclusive = right.upper_inclusive;
            }
            Ordering::Equal => left.upper_inclusive |= right.upper_inclusive,
            Ordering::Greater => {}
        },
    }
    Ok(left)
}

pub(super) fn range_text(
    range: &CanonicalRange,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    write_range(range, &mut output, control)?;
    Ok(output.finish()?)
}

pub(super) fn multirange_text(
    ranges: &CanonicalMultirange,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut output = ProductionString::new(*control);
    output.push('{')?;
    for (index, range) in ranges.ranges.iter().enumerate() {
        if index != 0 {
            output.push(',')?;
        }
        write_range(range, &mut output, control)?;
    }
    output.push('}')?;
    Ok(output.finish()?)
}

fn write_range(
    range: &CanonicalRange,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<()> {
    if range.empty {
        output.push_str("empty")?;
        return Ok(());
    }
    output.push(if range.lower_inclusive { '[' } else { '(' })?;
    if let Some(lower) = range.lower() {
        write_bound(lower, output, control)?;
    }
    output.push(',')?;
    if let Some(upper) = range.upper() {
        write_bound(upper, output, control)?;
    }
    output.push(if range.upper_inclusive { ']' } else { ')' })?;
    Ok(())
}

fn write_bound(
    value: &Value,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<()> {
    let raw = match value {
        Value::Int(value) => control.format(format_args!("{value}"))?,
        Value::Decimal(value) => value.to_sql_string_with_control(control)?,
        Value::Temporal(value) => value.to_sql_string_with_control(control)?,
        _ => {
            return Err(SQLError::Internal(
                "incompatible canonical range bound".into(),
            ))
        }
    };
    let mut quoted = raw.is_empty();
    for character in raw.chars() {
        control.check()?;
        quoted |= character.is_whitespace()
            || matches!(character, ',' | '[' | ']' | '(' | ')' | '"' | '\\');
    }
    if quoted {
        output.push('"')?;
    }
    for character in raw.chars() {
        if quoted && matches!(character, '\\' | '"') {
            output.push('\\')?;
        }
        output.push(character)?;
    }
    if quoted {
        output.push('"')?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
