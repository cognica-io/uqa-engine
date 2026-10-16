//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range parsing, normalization and text production share admitted constructors.

use super::{
    CanonicalMultirange, CanonicalRange, Ordering, ProductionControl, RangeSemantics, RangeSubtype,
    Result, SQLError, Value,
};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionString, ProductionVec},
    ordering::sort_by_with_control,
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
    let Some(input) = super::input::range(text, control)? else {
        return Ok(control.finish(CanonicalRange::empty(subtype), control.empty_reservation())?);
    };
    let semantics = RangeSemantics::native(subtype);
    let lower = parse_bound(input.lower, semantics, control)?;
    let upper = parse_bound(input.upper, semantics, control)?;
    let lower_inclusive = input.lower_inclusive && lower.is_some();
    let upper_inclusive = input.upper_inclusive && upper.is_some();
    let (lower, lower_memory) = lower.into_parts();
    let (upper, upper_memory) = upper.into_parts();
    let mut owned = RangeWorkspace {
        value: CanonicalRange {
            subtype: semantics.identity(),
            lower,
            upper,
            lower_inclusive,
            upper_inclusive,
            empty: false,
        },
        memory: control.combine(lower_memory, upper_memory),
    };
    let range = &mut owned.value;
    if bounds_are_empty(range, control)? {
        owned.value = CanonicalRange::empty(subtype);
        return Ok(control.finish(owned.value, owned.memory)?);
    }
    semantics.canonicalize(range)?;
    if bounds_are_empty(range, control)? {
        owned.value = CanonicalRange::empty(subtype);
    }
    Ok(control.finish(owned.value, owned.memory)?)
}

fn bounds_are_empty(range: &CanonicalRange, control: &ProductionControl<'_>) -> Result<bool> {
    Ok(match (&range.lower, &range.upper) {
        (Some(lower), Some(upper)) => match range.semantics().compare(lower, upper, control)? {
            Ordering::Greater => {
                return Err(SQLError::Routine {
                    sqlstate: "22000".into(),
                    message: "range lower bound must be less than or equal to range upper bound"
                        .into(),
                })
            }
            Ordering::Equal => !(range.lower_inclusive && range.upper_inclusive),
            Ordering::Less => false,
        },
        _ => false,
    })
}

pub(in crate::expr) fn parse_multirange_with_control(
    text: &str,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalMultirange>> {
    control.check()?;
    let mut input = super::input::MultirangeInput::new(text, control)?;
    let mut ranges = ProductionVec::new(*control);
    while let Some(item) = input.next_range(control)? {
        let range = parse_range_with_control(item, subtype, control)?;
        if !range.empty {
            ranges.push_produced(range)?;
        }
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
        let joins_previous = normalized
            .last()
            .map(|previous| joins(previous, &range, control))
            .transpose()?
            .unwrap_or(false);
        if joins_previous {
            let previous = normalized.pop().expect("previous range");
            normalized.push(merge_owned(previous, range, control)?);
            continue;
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

fn parse_bound(
    input: Option<super::input::BoundInput<'_>>,
    semantics: &RangeSemantics,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<Value>>> {
    let Some(input) = input else {
        return Ok(control.finish(None, control.empty_reservation())?);
    };
    let (text, memory) = input.decode(control)?.into_parts();
    let source = RangeWorkspace {
        value: Value::Str(text),
        memory,
    };
    let parsed = semantics.input(&source.value, control)?;
    let (value, memory) = parsed.into_parts();
    Ok(control.finish(Some(value), memory)?)
}

pub(super) fn compare_lower_bounds(
    left: &CanonicalRange,
    right: &CanonicalRange,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    super::comparison::compare_lower_bounds_by(left, right, control, |a, b, control| {
        left.semantics().compare(a, b, control)
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
        (Some(upper), Some(lower)) => match left.semantics().compare(upper, lower, control)? {
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
        if left.semantics().compare(a, b, control)? == Ordering::Equal {
            left.lower_inclusive |= right.lower_inclusive;
        }
    }
    match (left.upper(), right.upper()) {
        (None, _) => {}
        (_, None) => {
            left.upper = None;
            left.upper_inclusive = false;
        }
        (Some(a), Some(b)) => match left.semantics().compare(a, b, control)? {
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

pub(super) fn write_range(
    range: &CanonicalRange,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<()> {
    write_range_with(range, output, control, |value, control| {
        range.semantics().output(value, control)
    })
}

pub(super) fn write_range_with<I>(
    range: &CanonicalRange<I>,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
    render: impl Fn(&Value, &ProductionControl<'_>) -> Result<Produced<String>>,
) -> Result<()> {
    if range.empty {
        output.push_str("empty")?;
        return Ok(());
    }
    output.push(if range.lower_inclusive { '[' } else { '(' })?;
    if let Some(lower) = range.lower() {
        write_bound(&render(lower, control)?, output, control)?;
    }
    output.push(',')?;
    if let Some(upper) = range.upper() {
        write_bound(&render(upper, control)?, output, control)?;
    }
    output.push(if range.upper_inclusive { ']' } else { ')' })?;
    Ok(())
}

fn write_bound(
    raw: &str,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<()> {
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
