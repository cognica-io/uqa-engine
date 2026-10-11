//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ISO 8601 designator and alternative interval input, including `PostgreSQL`'s numeric conventions.

use super::{IntervalFields, IntervalInput, Quantity, TemporalInputError, TemporalValue, Unit};
use crate::{memory::ProductionControl, ValueRetentionError};

pub(super) fn parse(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<IntervalInput<TemporalValue>, ValueRetentionError> {
    let Some(mut text) = input.strip_prefix('P').filter(|s| !s.is_empty()) else {
        return Ok(Err(TemporalInputError::InvalidSyntax));
    };
    let mut date = true;
    let mut have_field = false;
    let mut fields = IntervalFields::default();
    while !text.is_empty() {
        control.check()?;
        if let Some(rest) = text.strip_prefix('T') {
            date = false;
            have_field = false;
            text = rest;
            continue;
        }
        let outcome = field(&mut text, date, have_field, &mut fields, control)?;
        match outcome {
            Ok(()) => have_field = true,
            Err(error) => return Ok(Err(error)),
        }
    }
    Ok(fields.finish())
}

fn field(
    text: &mut &str,
    date: bool,
    have_field: bool,
    fields: &mut IntervalFields,
    control: &ProductionControl<'_>,
) -> Result<IntervalInput<()>, ValueRetentionError> {
    let initial = *text;
    let number = match number(text, control)? {
        Ok(number) => number,
        Err(error) => return Ok(Err(error)),
    };
    let unit = text.as_bytes().first().copied();
    let named = match (date, unit) {
        (true, Some(b'Y')) => Some(Unit::Year),
        (true, Some(b'M')) => Some(Unit::Month),
        (true, Some(b'W')) => Some(Unit::Week),
        (true, Some(b'D')) => Some(Unit::Day),
        (false, Some(b'H')) => Some(Unit::Hour),
        (false, Some(b'M')) => Some(Unit::Minute),
        (false, Some(b'S')) => Some(Unit::Second),
        _ => None,
    };
    if let Some(unit) = named {
        *text = &text[1..];
        return Ok(fields.add(unit, number));
    }
    if have_field {
        return Ok(Err(TemporalInputError::InvalidSyntax));
    }
    let width = initial
        .trim_start_matches('-')
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if date && matches!(unit, None | Some(b'T')) && width == 8 {
        return Ok((|| {
            fields.add(Unit::Year, Quantity::integer(number.whole / 10_000))?;
            fields.add(Unit::Month, Quantity::integer(number.whole / 100 % 100))?;
            fields.add(
                Unit::Day,
                Quantity {
                    whole: number.whole % 100,
                    fraction: number.fraction,
                },
            )
        })());
    }
    if !date && unit.is_none() && width == 6 {
        return Ok((|| {
            fields.add(Unit::Hour, Quantity::integer(number.whole / 10_000))?;
            fields.add(Unit::Minute, Quantity::integer(number.whole / 100 % 100))?;
            fields.add(Unit::Second, Quantity::integer(number.whole % 100))?;
            // PostgreSQL's basic time form scales this fraction as microseconds.
            fields.add(
                Unit::Micros,
                Quantity {
                    whole: 0,
                    fraction: number.fraction,
                },
            )
        })());
    }
    alternative(text, date, number, fields, control)
}

fn alternative(
    text: &mut &str,
    date: bool,
    first: Quantity,
    fields: &mut IntervalFields,
    control: &ProductionControl<'_>,
) -> Result<IntervalInput<()>, ValueRetentionError> {
    let (units, separator) = if date {
        ([Unit::Year, Unit::Month, Unit::Day], '-')
    } else {
        ([Unit::Hour, Unit::Minute, Unit::Second], ':')
    };
    if let Err(error) = fields.add(units[0], first) {
        return Ok(Err(error));
    }
    for unit in &units[1..] {
        if text.is_empty() || date && text.starts_with('T') {
            return Ok(Ok(()));
        }
        let Some(rest) = text.strip_prefix(separator) else {
            return Ok(Err(TemporalInputError::InvalidSyntax));
        };
        *text = rest;
        let number = match number(text, control)? {
            Ok(number) => number,
            Err(error) => return Ok(Err(error)),
        };
        if let Err(error) = fields.add(*unit, number) {
            return Ok(Err(error));
        }
    }
    Ok(if text.is_empty() || date && text.starts_with('T') {
        Ok(())
    } else {
        Err(TemporalInputError::InvalidSyntax)
    })
}

fn number(
    text: &mut &str,
    control: &ProductionControl<'_>,
) -> Result<IntervalInput<Quantity>, ValueRetentionError> {
    let bytes = text.as_bytes();
    let mut end = usize::from(bytes.first() == Some(&b'-'));
    let start = end;
    let mut digits = 0;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        control.check()?;
        end += 1;
        digits += 1;
    }
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            control.check()?;
            end += 1;
            digits += 1;
        }
    }
    if digits == 0 || start >= bytes.len() {
        return Ok(Err(TemporalInputError::InvalidSyntax));
    }
    let significand_end = end;
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let exponent = end;
        end += 1;
        if matches!(bytes.get(end), Some(b'-' | b'+')) {
            end += 1;
        }
        let exponent_digits = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            control.check()?;
            end += 1;
        }
        if end == exponent_digits {
            end = exponent;
        }
    }
    let Ok(value) = text[..end].parse::<f64>() else {
        return Ok(Err(TemporalInputError::InvalidSyntax));
    };
    let nonzero = text[..significand_end]
        .bytes()
        .any(|c| matches!(c, b'1'..=b'9'));
    if !value.is_finite() || value.is_subnormal() || value == 0.0 && nonzero {
        return Ok(Err(TemporalInputError::InvalidSyntax));
    }
    if value.abs() > 1.0e15 {
        return Ok(Err(TemporalInputError::IntervalFieldOverflow));
    }
    let whole = value.trunc() as i64;
    *text = &text[end..];
    Ok(Ok(Quantity {
        whole,
        fraction: value - whole as f64,
    }))
}
