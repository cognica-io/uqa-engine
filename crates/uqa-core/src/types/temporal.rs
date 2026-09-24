//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL temporal values, parsing, formatting, and total ordering.

use super::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Ordering, Timelike};
use crate::{
    memory::{ProductionControl, ProductionString},
    ValueRetentionError,
};

mod keys;
mod production;

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
    pub fn parse_date(input: &str) -> Option<Self> {
        Self::try_parse_date(input).ok()
    }

    /// Parse a date while retaining whether the input format or a field value is invalid.
    pub fn try_parse_date(input: &str) -> Result<Self, chrono::ParseError> {
        let date = NaiveDate::parse_from_str(input.trim(), "%Y-%m-%d")?;
        let days = date.signed_duration_since(epoch_date()).num_days();
        Ok(Self::Date {
            days: i32::try_from(days)
                .expect("chrono's date range fits PostgreSQL's i32 day carrier"),
        })
    }

    pub fn parse_time(input: &str) -> Option<Self> {
        Self::parse_time_with_control(input, &ProductionControl::uncontrolled())
            .ok()
            .flatten()
    }

    pub fn parse_time_tz(input: &str) -> Option<Self> {
        Self::parse_time_tz_with_control(input, &ProductionControl::uncontrolled())
            .ok()
            .flatten()
    }

    pub fn parse_timestamp(input: &str) -> Option<Self> {
        let input = input.trim();
        if let Some(Self::Date { days }) = Self::parse_date(input) {
            return Some(Self::Timestamp {
                micros: i64::from(days) * MICROS_PER_DAY,
            });
        }
        parse_naive_datetime(input).map(|dt| Self::Timestamp {
            micros: dt.and_utc().timestamp_micros(),
        })
    }

    pub fn parse_timestamp_tz(input: &str) -> Option<Self> {
        let input = input.trim();
        if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
            return Some(Self::TimestampTz {
                micros: dt.timestamp_micros(),
            });
        }
        for fmt in [
            "%Y-%m-%d %H:%M:%S%.f%:z",
            "%Y-%m-%d %H:%M:%S%.f %:z",
            "%Y-%m-%dT%H:%M:%S%.f%:z",
            "%Y-%m-%d %H:%M%:z",
            "%Y-%m-%d %H:%M %:z",
            "%Y-%m-%dT%H:%M%:z",
            "%Y-%m-%d %H:%M:%S%.f%z",
            "%Y-%m-%d %H:%M:%S%.f %z",
            "%Y-%m-%dT%H:%M:%S%.f%z",
            // PostgreSQL text output uses a bare-hour offset (`+00`).
            "%Y-%m-%d %H:%M:%S%.f%#z",
            "%Y-%m-%dT%H:%M:%S%.f%#z",
        ] {
            if let Ok(dt) = DateTime::parse_from_str(input, fmt) {
                return Some(Self::TimestampTz {
                    micros: dt.timestamp_micros(),
                });
            }
        }
        Self::parse_timestamp(input).and_then(|value| match value {
            Self::Timestamp { micros } => Some(Self::TimestampTz { micros }),
            _ => None,
        })
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

fn parse_naive_time(input: &str) -> Option<NaiveTime> {
    for fmt in ["%H:%M:%S%.f", "%H:%M"] {
        if let Ok(time) = NaiveTime::parse_from_str(input, fmt) {
            return Some(time);
        }
    }
    None
}

fn parse_time_micros(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<i64>, ValueRetentionError> {
    if let Some(suffix) = input.strip_prefix("24:") {
        let text = control.format(format_args!("00:{suffix}"))?;
        return Ok(parse_naive_time(&text).and_then(|time| {
            (time.num_seconds_from_midnight() == 0 && time.nanosecond() == 0)
                .then_some(MICROS_PER_DAY)
        }));
    }
    Ok(parse_naive_time(input).map(time_to_micros))
}

fn parse_naive_datetime(input: &str) -> Option<NaiveDateTime> {
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(input, fmt) {
            return Some(dt);
        }
    }
    None
}

fn time_to_micros(time: NaiveTime) -> i64 {
    i64::from(time.num_seconds_from_midnight()) * MICROS_PER_SECOND
        + i64::from(time.nanosecond() / 1_000)
}

fn split_offset_suffix(input: &str) -> Option<(&str, i32)> {
    if let Some(body) = input.strip_suffix('Z') {
        return Some((body, 0));
    }
    let plus = input.rfind('+');
    let minus = input.rfind('-');
    let pos = match (plus, minus) {
        (Some(p), Some(m)) => Some(p.max(m)),
        (Some(p), None) => Some(p),
        (None, Some(m)) => Some(m),
        (None, None) => None,
    }?;
    let (body, offset) = input.split_at(pos);
    Some((body, parse_offset_minutes(offset)?))
}

fn parse_offset_minutes(offset: &str) -> Option<i32> {
    let sign = match offset.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = &offset[1..];
    let (hours, minutes) = if let Some((h, m)) = body.split_once(':') {
        (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?)
    } else if matches!(body.len(), 1 | 2) {
        (body.parse::<i32>().ok()?, 0)
    } else if body.len() == 4 {
        (
            body[..2].parse::<i32>().ok()?,
            body[2..].parse::<i32>().ok()?,
        )
    } else {
        return None;
    };
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 60 + minutes))
}

/// Parse a `PostgreSQL` interval literal into `(months, days, micros)`.
fn parse_interval_literal(
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<TemporalValue>, ValueRetentionError> {
    let mut text = ProductionString::new(*control);
    for character in input.trim().chars() {
        text.push(character.to_ascii_lowercase())?;
    }
    let value = parse_interval_tokens(&text, control);
    control.check()?;
    Ok(value)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves temporal token error order"
)]
fn parse_interval_tokens(input: &str, control: &ProductionControl<'_>) -> Option<TemporalValue> {
    #[derive(Default)]
    struct Acc {
        months: i64,
        days: i64,
        micros: i64,
    }
    impl Acc {
        fn add_months(&mut self, value: f64) -> bool {
            let Some(value) = rounded_f64_to_i64(value) else {
                return false;
            };
            let Some(total) = self.months.checked_add(value) else {
                return false;
            };
            self.months = total;
            true
        }

        fn add_days(&mut self, value: f64) -> bool {
            let Some(value) = truncated_f64_to_i64(value) else {
                return false;
            };
            let Some(total) = self.days.checked_add(value) else {
                return false;
            };
            self.days = total;
            true
        }

        fn add_rounded_days(&mut self, value: f64) -> bool {
            let Some(value) = rounded_f64_to_i64(value) else {
                return false;
            };
            let Some(total) = self.days.checked_add(value) else {
                return false;
            };
            self.days = total;
            true
        }

        fn add_micros(&mut self, value: f64) -> bool {
            let Some(value) = rounded_f64_to_i64(value) else {
                return false;
            };
            self.add_micros_exact(value)
        }

        fn add_micros_exact(&mut self, value: i64) -> bool {
            let Some(total) = self.micros.checked_add(value) else {
                return false;
            };
            self.micros = total;
            true
        }

        // Carry a fractional remainder downward exactly like
        // PostgreSQL: month fractions become days (x30), day/week
        // fractions become microseconds (x86400s).
        fn add_unit(&mut self, unit: &str, quantity: f64) -> bool {
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
                    self.add_days(whole) && self.add_micros(frac * MICROS_PER_DAY as f64)
                }
                "week" | "weeks" | "w" => {
                    let total_days = quantity * 7.0;
                    self.add_days(total_days.trunc())
                        && self
                            .add_micros((total_days - total_days.trunc()) * MICROS_PER_DAY as f64)
                }
                "month" | "months" | "mon" | "mons" => {
                    self.add_months(whole) && self.add_rounded_days(frac * 30.0)
                }
                "year" | "years" | "yr" | "yrs" | "y" => self.add_months(quantity * 12.0),
                "decade" | "decades" => self.add_months(quantity * 120.0),
                "century" | "centuries" => self.add_months(quantity * 1_200.0),
                "millennium" | "millenniums" | "millennia" => self.add_months(quantity * 12_000.0),
                _ => false,
            }
        }
    }

    let mut text = input;
    let mut negate_all = false;
    if let Some(stripped) = text.strip_suffix("ago") {
        negate_all = true;
        text = stripped.trim_end();
    }
    if text.is_empty() {
        return None;
    }
    let mut acc = Acc::default();
    let mut pending: Option<f64> = None;
    for token in text.split_whitespace() {
        if control.check_cancellation().is_err() {
            return None;
        }
        if let Some(rest) = parse_interval_time_token(token) {
            // `HH:MM[:SS[.frac]]` (or `[+-]HH:MM...`) time-of-day part.
            // A bare number right before it is a day count
            // (`'3 4:05:06'` = 3 days 04:05:06).
            if let Some(days) = pending.take() {
                if !acc.add_days(days.trunc())
                    || !acc.add_micros((days - days.trunc()) * MICROS_PER_DAY as f64)
                {
                    return None;
                }
            }
            if !acc.add_micros_exact(rest) {
                return None;
            }
            continue;
        }
        if let Some((y, m)) = parse_interval_year_month_token(token) {
            let months = y.checked_mul(12)?.checked_add(m)?;
            acc.months = acc.months.checked_add(months)?;
            continue;
        }
        if let Ok(number) = token.parse::<f64>() {
            if !number.is_finite() {
                return None;
            }
            if let Some(prev) = pending.take() {
                // Two bare numbers in a row: the first was seconds.
                if !acc.add_micros(prev * MICROS_PER_SECOND as f64) {
                    return None;
                }
            }
            pending = Some(number);
            continue;
        }
        let quantity = pending.take().unwrap_or(1.0);
        if !acc.add_unit(token, quantity) {
            return None;
        }
    }
    if let Some(number) = pending {
        // Trailing bare number: PostgreSQL reads it as seconds.
        if !acc.add_micros(number * MICROS_PER_SECOND as f64) {
            return None;
        }
    }
    if negate_all {
        acc.months = acc.months.checked_neg()?;
        acc.days = acc.days.checked_neg()?;
        acc.micros = acc.micros.checked_neg()?;
    }
    Some(TemporalValue::Interval {
        months: i32::try_from(acc.months).ok()?,
        days: i32::try_from(acc.days).ok()?,
        micros: acc.micros,
    })
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

/// `[+-]HH:MM[:SS[.frac]]` -> signed microseconds. Rejects minute or
/// second fields of 60 or more, mirroring `PostgreSQL`.
fn parse_interval_time_token(token: &str) -> Option<i64> {
    if !token.contains(':') {
        return None;
    }
    let (sign, body) = match token.as_bytes().first()? {
        b'-' => (-1i64, &token[1..]),
        b'+' => (1, &token[1..]),
        _ => (1, token),
    };
    let mut parts = body.split(':');
    let hours: i64 = parts.next()?.parse().ok()?;
    let minutes: i64 = parts.next()?.parse().ok()?;
    let seconds = parts.next();
    if parts.next().is_some() {
        return None;
    }
    if !(0..60).contains(&minutes) {
        return None;
    }
    let mut micros = hours
        .checked_mul(3_600)?
        .checked_mul(MICROS_PER_SECOND)?
        .checked_add(minutes.checked_mul(60)?.checked_mul(MICROS_PER_SECOND)?)?;
    if let Some(seconds) = seconds {
        let seconds: f64 = seconds.parse().ok()?;
        if !(0.0..60.0).contains(&seconds) {
            return None;
        }
        micros = micros.checked_add(rounded_f64_to_i64(seconds * MICROS_PER_SECOND as f64)?)?;
    }
    sign.checked_mul(micros)
}

/// SQL-standard year-month literal `[+-]Y-M` -> `(years, months)`.
fn parse_interval_year_month_token(token: &str) -> Option<(i64, i64)> {
    let (sign, body) = match token.as_bytes().first()? {
        b'-' => (-1i64, &token[1..]),
        b'+' => (1, &token[1..]),
        _ => (1, token),
    };
    let (y, m) = body.split_once('-')?;
    if y.is_empty() || m.is_empty() || !y.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let years: i64 = y.parse().ok()?;
    let months: i64 = m.parse().ok()?;
    if !(0..12).contains(&months) {
        return None;
    }
    Some((sign.checked_mul(years)?, sign.checked_mul(months)?))
}
