//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range parsing, normalization and text production share admitted constructors.

#[cfg(test)]
use super::construction::RangeWorkspace;
use super::{
    CanonicalMultirange, CanonicalRange, ProductionControl, RangeSemantics, RangeSubtype, Result,
    Value,
};
#[cfg(test)]
use uqa_core::memory::ProductionVec;
use uqa_core::memory::{MemoryReservation, Produced, ProductionString};

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
    super::construction::parse_range_with_operations(text, RangeSemantics::native(subtype), control)
}

pub(in crate::expr) fn parse_multirange_with_control(
    text: &str,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalMultirange>> {
    super::construction::parse_multirange_with_operations(
        text,
        RangeSemantics::native(subtype),
        control,
    )
}

pub(in crate::expr) fn range_from_values_with_control(
    lower: &Value,
    upper: &Value,
    flags: (bool, bool),
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalRange>> {
    let semantics = RangeSemantics::native(subtype);
    let bound = |value: &Value| -> Result<Produced<Option<Value>>> {
        if matches!(value, Value::Null) {
            return Ok(control.finish(None, control.empty_reservation())?);
        }
        let (value, memory) = semantics.input(value, control)?.into_parts();
        Ok(control.finish(Some(value), memory)?)
    };
    let lower = bound(lower)?;
    let upper = bound(upper)?;
    super::construction::range_from_bounds_with_operations(
        lower, upper, flags.0, flags.1, semantics, control,
    )
}

pub(in crate::expr) fn multirange_from_produced_ranges(
    subtype: RangeSubtype,
    ranges: Produced<Vec<CanonicalRange>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalMultirange>> {
    super::construction::multirange_from_ranges_with_operations(
        ranges,
        RangeSemantics::native(subtype),
        control,
    )
}

pub(super) fn normalize_ranges(
    ranges: impl IntoIterator<Item = CanonicalRange>,
    subtype: RangeSubtype,
    control: &ProductionControl<'_>,
    memory: Option<MemoryReservation>,
) -> Result<Produced<CanonicalMultirange>> {
    super::construction::normalize_ranges(ranges, RangeSemantics::native(subtype), control, memory)
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
        quoted |= super::input::syntax_whitespace(character)
            || matches!(character, ',' | '[' | ']' | '(' | ')' | '"' | '\\');
    }
    if quoted {
        output.push('"')?;
    }
    for character in raw.chars() {
        if quoted && matches!(character, '\\' | '"') {
            output.push(character)?;
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
