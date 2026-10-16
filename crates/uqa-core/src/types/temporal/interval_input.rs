//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked interval input under an explicit presentation style.

use super::{
    input, TemporalInputError, TemporalIntervalStyle, TemporalValue, MICROS_PER_DAY,
    MICROS_PER_SECOND,
};
use crate::{
    memory::{ProductionControl, ProductionString},
    ValueRetentionError,
};

mod iso;
mod quantity;
use quantity::{IntervalFields, Quantity, Unit};

/// Read a `PostgreSQL` interval literal as `interval_in` reads the traditional and SQL standard forms (`'1 day'`, `'90 minutes'`, `'1 day 3 hours'`, `'1-2'`, `'3 4:05:06'`, bare seconds, a leading `@`, a trailing `ago`), keeping `DecodeInterval`'s distinctions: text that is not an interval is a syntax error and a field or quantity that does not fit is an interval field overflow. Fractional quantities cascade into the next smaller unit (`'1.5 mons'` is `1 mon 15 days`).
pub(super) fn parse_interval_literal(
    input: &str,
    style: TemporalIntervalStyle,
    control: &ProductionControl<'_>,
) -> Result<Result<TemporalValue, TemporalInputError>, ValueRetentionError> {
    control.check()?;
    if let Some(infinity) = input::infinite_token(input.trim()) {
        return Ok(Ok(infinity.interval()));
    }
    let mut text = ProductionString::new(*control);
    for character in input.trim().chars() {
        text.push(character.to_ascii_lowercase())?;
    }
    let value = parse_interval_tokens(&text, style, control)?;
    let value = if matches!(value, Err(TemporalInputError::InvalidSyntax)) {
        iso::parse(input, control)?
    } else {
        value
    };
    control.check()?;
    Ok(value)
}

type IntervalInput<T> = Result<T, TemporalInputError>;

fn parse_interval_tokens(
    input: &str,
    style: TemporalIntervalStyle,
    control: &ProductionControl<'_>,
) -> Result<IntervalInput<TemporalValue>, ValueRetentionError> {
    let (text, negate_all) = input
        .strip_suffix("ago")
        .map_or((input, false), |body| (body.trim_end(), true));
    let tokens = || {
        text.split_whitespace()
            .map(|s| s.trim_start_matches('@'))
            .filter(|s| !s.is_empty())
    };
    let force_negative = style == TemporalIntervalStyle::SqlStandard
        && tokens().next().is_some_and(|s| s.starts_with('-'))
        && !tokens().skip(1).any(|s| s.starts_with(['-', '+']));
    let mut fields = IntervalFields::default();
    let mut unit = Unit::Second;
    let mut pending_unit = false;
    let mut seen = 0_u16;
    // A quantity takes its unit from the token to its right. This also preserves
    // PostgreSQL's overflow and duplicate-field diagnostic order.
    for token in tokens().rev() {
        control.check()?;
        let outcome = interval_token(
            token,
            &mut fields,
            &mut unit,
            &mut pending_unit,
            force_negative,
        );
        match outcome {
            Ok(mask) if seen & mask == 0 => seen |= mask,
            Ok(_) => return Ok(Err(TemporalInputError::InvalidSyntax)),
            Err(error) => return Ok(Err(error)),
        }
    }
    Ok((|| {
        if seen == 0 || pending_unit {
            return Err(TemporalInputError::InvalidSyntax);
        }
        if negate_all {
            fields.negate()?;
        }
        fields.finish()
    })())
}

fn interval_token(
    token: &str,
    fields: &mut IntervalFields,
    unit: &mut Unit,
    pending_unit: &mut bool,
    force_negative: bool,
) -> IntervalInput<u16> {
    if let Some(micros) = parse_interval_time_token(token)? {
        let micros = if force_negative && micros > 0 {
            -micros
        } else {
            micros
        };
        fields.time(micros, 0.0, 1)?;
        *unit = Unit::Day;
        *pending_unit = false;
        return Ok(Unit::TIME_MASK);
    }
    if let Some((years, months)) = parse_interval_year_month_token(token)? {
        let total = years
            .checked_mul(12)
            .and_then(|value| value.checked_add(months))
            .ok_or(TemporalInputError::IntervalFieldOverflow)?;
        let quantity = Quantity::integer(total).force_negative(force_negative);
        fields.add(Unit::Month, quantity)?;
        *unit = Unit::Month;
        *pending_unit = false;
        return Ok(unit.mask(quantity));
    }
    if token
        .as_bytes()
        .first()
        .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.'))
    {
        let quantity = Quantity::decimal(token)?.force_negative(force_negative);
        fields.add(*unit, quantity)?;
        let mask = unit.mask(quantity);
        if matches!(unit, Unit::Hour) {
            *unit = Unit::Day;
        }
        *pending_unit = false;
        return Ok(mask);
    }
    if *pending_unit {
        return Err(TemporalInputError::InvalidSyntax);
    }
    *unit = Unit::parse(token)?;
    *pending_unit = true;
    Ok(0)
}

/// `[+-]HH:MM[:SS[.frac]]` as signed microseconds, or `None` for a token that is no time of day. A minute past 59 or a second past 60 is an interval field overflow, as `DecodeTime` reports it for intervals; a sixtieth second carries into the next minute.
fn parse_interval_time_token(token: &str) -> IntervalInput<Option<i64>> {
    if !token.contains(':') {
        return Ok(None);
    }
    let (sign, body) = match token.as_bytes().first() {
        Some(b'-') => (-1i64, &token[1..]),
        Some(b'+') => (1, &token[1..]),
        _ => (1, token),
    };
    let parsed = parse_interval_time_fields(body);
    // DecodeInterval retries a failed signed time as a signed number. A valid
    // leading integer leaves the colon as invalid syntax; integer overflow
    // retains its field-overflow diagnostic.
    let micros = parsed.map_err(|error| {
        if body.len() != token.len()
            && token
                .split(':')
                .next()
                .is_some_and(|hours| hours.parse::<i64>().is_ok())
        {
            TemporalInputError::InvalidSyntax
        } else {
            error
        }
    })?;
    Ok(Some(
        sign.checked_mul(micros)
            .ok_or(TemporalInputError::IntervalFieldOverflow)?,
    ))
}

fn parse_interval_time_fields(body: &str) -> IntervalInput<i64> {
    let integer = |text: &str| {
        text.parse::<i64>().map_err(|error| match error.kind() {
            std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => {
                TemporalInputError::IntervalFieldOverflow
            }
            _ => TemporalInputError::InvalidSyntax,
        })
    };
    let mut parts = body.split(':');
    let mut hours = integer(parts.next().ok_or(TemporalInputError::InvalidSyntax)?)?;
    let next = parts.next().ok_or(TemporalInputError::InvalidSyntax)?;
    let third = parts.next();
    if parts.next().is_some() {
        return Err(TemporalInputError::InvalidSyntax);
    }
    let (minutes, seconds) = if third.is_none() && next.contains('.') {
        let minutes = hours;
        hours = 0;
        (minutes, Quantity::decimal(next)?)
    } else {
        (
            integer(next)?,
            third
                .map(Quantity::decimal)
                .transpose()?
                .unwrap_or(Quantity::integer(0)),
        )
    };
    if hours < 0
        || !(0..60).contains(&minutes)
        || !(0..=60).contains(&seconds.whole)
        || seconds.fraction < 0.0
    {
        return Err(TemporalInputError::IntervalFieldOverflow);
    }
    let fractional_micros = (seconds.fraction * MICROS_PER_SECOND as f64).round_ties_even() as i64;
    hours
        .checked_mul(3_600)
        .and_then(|v| v.checked_mul(MICROS_PER_SECOND))
        .and_then(|v| v.checked_add(minutes * 60 * MICROS_PER_SECOND))
        .and_then(|v| v.checked_add(seconds.whole * MICROS_PER_SECOND))
        .and_then(|v| v.checked_add(fractional_micros))
        .ok_or(TemporalInputError::IntervalFieldOverflow)
}

/// The SQL standard year-month literal `[+-]Y-M` as `(years, months)`, or `None` for a token of another shape; a month past 11 is an interval field overflow.
fn parse_interval_year_month_token(token: &str) -> IntervalInput<Option<(i64, i64)>> {
    let (sign, body) = match token.as_bytes().first() {
        Some(b'-') => (-1i64, &token[1..]),
        Some(b'+') => (1, &token[1..]),
        _ => (1, token),
    };
    let Some((years, months)) = body.split_once('-') else {
        return Ok(None);
    };
    if years.is_empty()
        || months.is_empty()
        || !years.bytes().all(|byte| byte.is_ascii_digit())
        || !months.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Ok(None);
    }
    let years: i64 = years
        .parse()
        .map_err(|_| TemporalInputError::IntervalFieldOverflow)?;
    let months: i64 = months
        .parse()
        .map_err(|_| TemporalInputError::IntervalFieldOverflow)?;
    if !(0..12).contains(&months) {
        return Err(TemporalInputError::IntervalFieldOverflow);
    }
    let overflow = || TemporalInputError::IntervalFieldOverflow;
    Ok(Some((
        sign.checked_mul(years).ok_or_else(overflow)?,
        sign.checked_mul(months).ok_or_else(overflow)?,
    )))
}
