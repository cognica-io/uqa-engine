//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL temporal values, parsing, formatting, and total ordering.

use super::{DateTime, Duration, NaiveDate, Ordering};
use crate::{
    memory::{ProductionControl, ProductionString},
    ValueRetentionError,
};

mod input;
mod keys;
mod production;
mod timezone;

pub use input::{TemporalDateOrder, TemporalInputError};
pub use timezone::TemporalTimeZone;

pub(super) const MICROS_PER_SECOND: i64 = 1_000_000;
pub(super) const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;

/// Compact temporal values used by SQL `DATE`, `TIME`, and `TIMESTAMP`
/// columns. The payload is numeric so comparison and sorting do not
/// depend on string collation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "$uqa_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TemporalValue {
    Date {
        days: i32,
    },
    Time {
        micros: i64,
    },
    TimeTz {
        micros: i64,
        offset_minutes: i32,
    },
    Timestamp {
        micros: i64,
    },
    TimestampTz {
        micros: i64,
    },
    /// `INTERVAL` values use `PostgreSQL`'s exact three-field model:
    /// months and days stay symbolic (a month is not a fixed number of
    /// days) while sub-day amounts collapse into microseconds.
    Interval {
        months: i32,
        days: i32,
        micros: i64,
    },
}

impl TemporalValue {
    /// Test a finite Unix-microsecond timestamp against `PostgreSQL`'s Julian range. The epoch conversion uses a wider integer because `PostgreSQL`'s upper bound lies beyond this carrier's largest Unix-microsecond value.
    #[must_use]
    pub const fn timestamp_micros_in_range(micros: i64) -> bool {
        let postgres_micros = micros as i128 - 946_684_800_000_000;
        postgres_micros >= -211_813_488_000_000_000 && postgres_micros < 9_223_371_331_200_000_000
    }

    /// The `parse_*` readers are the input functions with the wall clock standing in for the transaction start that `now` and `today` name; a text the type rejects reads as `None`.
    pub fn parse_date(input: &str) -> Option<Self> {
        Self::date_input(input, wall_clock_micros()).ok()
    }

    pub fn parse_time(input: &str) -> Option<Self> {
        Self::time_input(input, wall_clock_micros()).ok()
    }

    pub fn parse_time_tz(input: &str) -> Option<Self> {
        Self::time_tz_input(input, wall_clock_micros()).ok()
    }

    pub fn parse_timestamp(input: &str) -> Option<Self> {
        Self::timestamp_input(input, wall_clock_micros()).ok()
    }

    pub fn parse_timestamp_tz(input: &str) -> Option<Self> {
        Self::timestamp_tz_input(input, wall_clock_micros()).ok()
    }

    pub fn parse_same_kind(&self, input: &str) -> Option<Self> {
        self.parse_same_kind_with_control(input, &ProductionControl::uncontrolled())
            .expect("ordinary temporal kind parser")
    }

    /// Parse a `PostgreSQL` interval literal (`'1 day'`, `'90 minutes'`,
    /// `'1 day 3 hours'`, `'1-2'`, `'3 4:05:06'`, bare seconds, `ago`).
    /// Fractional quantities cascade into the next-smaller unit exactly
    /// like `PostgreSQL` (`'1.5 mons'` -> `1 mon 15 days`).
    pub fn parse_interval(input: &str) -> Option<Self> {
        Self::parse_interval_with_control(input, &ProductionControl::uncontrolled())
            .ok()
            .flatten()
    }

    pub fn to_sql_string(&self) -> String {
        self.to_sql_string_with_control(&ProductionControl::uncontrolled())
            .expect("ordinary temporal formatting")
            .into_uncontrolled()
            .expect("ordinary temporal text")
    }

    fn sort_key(&self) -> (u8, i128, i64) {
        match self {
            Self::Date { days } => (0, i128::from(*days), 0),
            Self::Time { micros } => (1, i128::from(*micros), 0),
            Self::TimeTz {
                micros,
                offset_minutes,
            } => (
                2,
                i128::from(*micros)
                    - i128::from(*offset_minutes) * 60 * i128::from(MICROS_PER_SECOND),
                // PostgreSQL compares seconds west of UTC after adjusted time; our carrier stores minutes east. Widen before negation for arbitrary deserialized carriers.
                -i64::from(*offset_minutes),
            ),
            Self::Timestamp { micros } => (3, i128::from(*micros), 0),
            Self::TimestampTz { micros } => (4, i128::from(*micros), 0),
            // PostgreSQL's interval_cmp flattens to microseconds with
            // 30-day months for ordering purposes.
            Self::Interval {
                months,
                days,
                micros,
            } => (
                5,
                (i128::from(*months) * 30 + i128::from(*days)) * i128::from(MICROS_PER_DAY)
                    + i128::from(*micros),
                0,
            ),
        }
    }
}

impl PartialEq for TemporalValue {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for TemporalValue {}

impl PartialOrd for TemporalValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TemporalValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

fn epoch_date() -> NaiveDate {
    DateTime::<chrono::Utc>::UNIX_EPOCH.date_naive()
}

/// The platform clock in Unix microseconds, which stands in for the transaction start when no statement supplies one.
fn wall_clock_micros() -> i64 {
    chrono::Utc::now().timestamp_micros()
}

/// Read a `PostgreSQL` interval literal as `interval_in` reads the traditional and SQL standard forms (`'1 day'`, `'90 minutes'`, `'1 day 3 hours'`, `'1-2'`, `'3 4:05:06'`, bare seconds, a leading `@`, a trailing `ago`), keeping `DecodeInterval`'s distinctions: text that is not an interval is a syntax error and a field or quantity that does not fit is an interval field overflow. Fractional quantities cascade into the next smaller unit (`'1.5 mons'` is `1 mon 15 days`).
fn parse_interval_literal(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Result<TemporalValue, TemporalInputError>, ValueRetentionError> {
    let mut text = ProductionString::new(*control);
    for character in input.trim().chars() {
        text.push(character.to_ascii_lowercase())?;
    }
    let value = parse_interval_tokens(&text, control)?;
    control.check()?;
    Ok(value)
}

type IntervalInput<T> = Result<T, TemporalInputError>;

/// The accumulating fields of an interval, each addition checked for the carrier's range.
#[derive(Default)]
struct IntervalFields {
    months: i64,
    days: i64,
    micros: i64,
}

impl IntervalFields {
    fn add_months(&mut self, value: f64) -> IntervalInput<()> {
        self.months = checked_sum(self.months, rounded_f64_to_i64(value))?;
        Ok(())
    }

    fn add_days(&mut self, value: f64) -> IntervalInput<()> {
        self.days = checked_sum(self.days, truncated_f64_to_i64(value))?;
        Ok(())
    }

    fn add_rounded_days(&mut self, value: f64) -> IntervalInput<()> {
        self.days = checked_sum(self.days, rounded_f64_to_i64(value))?;
        Ok(())
    }

    fn add_micros(&mut self, value: f64) -> IntervalInput<()> {
        self.micros = checked_sum(self.micros, rounded_f64_to_i64(value))?;
        Ok(())
    }

    fn add_micros_exact(&mut self, value: i64) -> IntervalInput<()> {
        self.micros = checked_sum(self.micros, Some(value))?;
        Ok(())
    }

    /// Add `quantity` of `unit`, carrying a fractional remainder downward as `PostgreSQL` does: month fractions become days (x30), day and week fractions become microseconds. A word that names no unit is a syntax error.
    fn add_unit(&mut self, unit: &str, quantity: f64) -> IntervalInput<()> {
        const MICROS_PER_HOUR: i64 = 3_600 * MICROS_PER_SECOND;
        const MICROS_PER_MINUTE: i64 = 60 * MICROS_PER_SECOND;
        let whole = quantity.trunc();
        let frac = quantity - whole;
        match unit {
            "microsecond" | "microseconds" | "us" => self.add_micros(quantity),
            "millisecond" | "milliseconds" | "ms" => self.add_micros(quantity * 1_000.0),
            "second" | "seconds" | "sec" | "secs" | "s" => {
                self.add_micros(quantity * MICROS_PER_SECOND as f64)
            }
            "minute" | "minutes" | "min" | "mins" | "m" => {
                self.add_micros(quantity * MICROS_PER_MINUTE as f64)
            }
            "hour" | "hours" | "hr" | "hrs" | "h" => {
                self.add_micros(quantity * MICROS_PER_HOUR as f64)
            }
            "day" | "days" | "d" => {
                self.add_days(whole)?;
                self.add_micros(frac * MICROS_PER_DAY as f64)
            }
            "week" | "weeks" | "w" => {
                let total_days = quantity * 7.0;
                self.add_days(total_days.trunc())?;
                self.add_micros((total_days - total_days.trunc()) * MICROS_PER_DAY as f64)
            }
            "month" | "months" | "mon" | "mons" => {
                self.add_months(whole)?;
                self.add_rounded_days(frac * 30.0)
            }
            "year" | "years" | "yr" | "yrs" | "y" => self.add_months(quantity * 12.0),
            "decade" | "decades" => self.add_months(quantity * 120.0),
            "century" | "centuries" => self.add_months(quantity * 1_200.0),
            "millennium" | "millenniums" | "millennia" => self.add_months(quantity * 12_000.0),
            _ => Err(TemporalInputError::InvalidSyntax),
        }
    }

    fn negate(&mut self) -> IntervalInput<()> {
        self.months = self
            .months
            .checked_neg()
            .ok_or(TemporalInputError::IntervalFieldOverflow)?;
        self.days = self
            .days
            .checked_neg()
            .ok_or(TemporalInputError::IntervalFieldOverflow)?;
        self.micros = self
            .micros
            .checked_neg()
            .ok_or(TemporalInputError::IntervalFieldOverflow)?;
        Ok(())
    }

    fn finish(self) -> IntervalInput<TemporalValue> {
        Ok(TemporalValue::Interval {
            months: i32::try_from(self.months)
                .map_err(|_| TemporalInputError::IntervalFieldOverflow)?,
            days: i32::try_from(self.days)
                .map_err(|_| TemporalInputError::IntervalFieldOverflow)?,
            micros: self.micros,
        })
    }
}

fn checked_sum(total: i64, value: Option<i64>) -> IntervalInput<i64> {
    value
        .and_then(|value| total.checked_add(value))
        .ok_or(TemporalInputError::IntervalFieldOverflow)
}

fn parse_interval_tokens(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<IntervalInput<TemporalValue>, ValueRetentionError> {
    let mut text = input;
    let mut negate_all = false;
    if let Some(stripped) = text.strip_suffix("ago") {
        negate_all = true;
        text = stripped.trim_end();
    }
    if text.is_empty() {
        return Ok(Err(TemporalInputError::InvalidSyntax));
    }
    let mut fields = IntervalFields::default();
    let mut pending: Option<f64> = None;
    for token in text.split_whitespace() {
        control.check()?;
        // `ParseDateTime` passes over the `@` of the verbose form.
        if token == "@" {
            continue;
        }
        let token = token.strip_prefix('@').unwrap_or(token);
        let outcome = interval_token(token, &mut fields, &mut pending);
        if let Err(error) = outcome {
            return Ok(Err(error));
        }
    }
    let finished = (|| {
        if let Some(number) = pending {
            // A trailing bare number is seconds.
            fields.add_micros(number * MICROS_PER_SECOND as f64)?;
        }
        if negate_all {
            fields.negate()?;
        }
        fields.finish()
    })();
    Ok(finished)
}

/// Fold one token into the fields: a time of day, a year-month pair, a bare quantity awaiting its unit, or a unit word.
fn interval_token(
    token: &str,
    fields: &mut IntervalFields,
    pending: &mut Option<f64>,
) -> IntervalInput<()> {
    if let Some(micros) = parse_interval_time_token(token)? {
        // `HH:MM[:SS[.frac]]`; a bare number right before it is a day count (`'3 4:05:06'` is 3 days 04:05:06).
        if let Some(days) = pending.take() {
            fields.add_days(days.trunc())?;
            fields.add_micros((days - days.trunc()) * MICROS_PER_DAY as f64)?;
        }
        return fields.add_micros_exact(micros);
    }
    if let Some((years, months)) = parse_interval_year_month_token(token)? {
        let total = years
            .checked_mul(12)
            .and_then(|value| value.checked_add(months))
            .ok_or(TemporalInputError::IntervalFieldOverflow)?;
        fields.months = checked_sum(fields.months, Some(total))?;
        return Ok(());
    }
    if let Ok(number) = token.parse::<f64>() {
        if !number.is_finite() {
            return Err(TemporalInputError::InvalidSyntax);
        }
        // Two bare numbers in a row name no unit for the first, as `DecodeInterval` rejects.
        if pending.is_some() {
            return Err(TemporalInputError::InvalidSyntax);
        }
        *pending = Some(number);
        return Ok(());
    }
    let quantity = pending.take().unwrap_or(1.0);
    fields.add_unit(token, quantity)
}

fn truncated_f64_to_i64(value: f64) -> Option<i64> {
    const I64_UPPER_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    const I64_LOWER_INCLUSIVE: f64 = -9_223_372_036_854_775_808.0;
    let value = value.trunc();
    if !(I64_LOWER_INCLUSIVE..I64_UPPER_EXCLUSIVE).contains(&value) {
        return None;
    }
    Some(value as i64)
}

fn rounded_f64_to_i64(value: f64) -> Option<i64> {
    truncated_f64_to_i64(value.round())
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
    let mut parts = body.split(':');
    let hours: i64 = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or(TemporalInputError::InvalidSyntax)?;
    let minutes: i64 = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or(TemporalInputError::InvalidSyntax)?;
    let seconds = parts.next();
    if parts.next().is_some() {
        return Err(TemporalInputError::InvalidSyntax);
    }
    if !(0..60).contains(&minutes) {
        return Err(TemporalInputError::IntervalFieldOverflow);
    }
    let overflow = || TemporalInputError::IntervalFieldOverflow;
    let mut micros = hours
        .checked_mul(3_600)
        .and_then(|value| value.checked_mul(MICROS_PER_SECOND))
        .and_then(|value| value.checked_add(minutes * 60 * MICROS_PER_SECOND))
        .ok_or_else(overflow)?;
    if let Some(seconds) = seconds {
        let seconds: f64 = seconds
            .parse()
            .map_err(|_| TemporalInputError::InvalidSyntax)?;
        if !(0.0..=60.0).contains(&seconds) {
            return Err(TemporalInputError::IntervalFieldOverflow);
        }
        micros = micros
            .checked_add(
                rounded_f64_to_i64(seconds * MICROS_PER_SECOND as f64).ok_or_else(overflow)?,
            )
            .ok_or_else(overflow)?;
    }
    Ok(Some(sign.checked_mul(micros).ok_or_else(overflow)?))
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
