//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL temporal values, parsing, formatting, and total ordering.

use super::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Ordering, Timelike};

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
        parse_naive_time(input.trim()).map(|time| Self::Time {
            micros: time_to_micros(time),
        })
    }

    pub fn parse_time_tz(input: &str) -> Option<Self> {
        let (time, offset_minutes) = split_offset_suffix(input.trim())?;
        parse_naive_time(time.trim()).map(|time| Self::TimeTz {
            micros: time_to_micros(time),
            offset_minutes,
        })
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
        match self {
            Self::Date { .. } => Self::parse_date(input),
            Self::Time { .. } => Self::parse_time(input),
            Self::TimeTz { .. } => Self::parse_time_tz(input),
            Self::Timestamp { .. } => Self::parse_timestamp(input),
            Self::TimestampTz { .. } => Self::parse_timestamp_tz(input),
            Self::Interval { .. } => Self::parse_interval(input),
        }
    }

    /// Parse a `PostgreSQL` interval literal (`'1 day'`, `'90 minutes'`,
    /// `'1 day 3 hours'`, `'1-2'`, `'3 4:05:06'`, bare seconds, `ago`).
    /// Fractional quantities cascade into the next-smaller unit exactly
    /// like `PostgreSQL` (`'1.5 mons'` -> `1 mon 15 days`).
    pub fn parse_interval(input: &str) -> Option<Self> {
        parse_interval_literal(input)
    }

    pub fn to_sql_string(&self) -> String {
        match self {
            Self::Date { days } => epoch_date()
                .checked_add_signed(Duration::days(i64::from(*days)))
                .map_or_else(|| days.to_string(), |date| date.to_string()),
            Self::Time { micros } => format_time_micros(*micros),
            Self::TimeTz {
                micros,
                offset_minutes,
            } => format!(
                "{}{}",
                format_time_micros(*micros),
                format_offset(*offset_minutes)
            ),
            Self::Timestamp { micros } => format_timestamp_micros(*micros, false),
            Self::TimestampTz { micros } => format_timestamp_micros(*micros, true),
            Self::Interval {
                months,
                days,
                micros,
            } => format_interval(*months, *days, *micros),
        }
    }

    fn sort_key(&self) -> (u8, i128) {
        match self {
            Self::Date { days } => (0, i128::from(*days)),
            Self::Time { micros } => (
                1,
                i128::from(*micros).rem_euclid(i128::from(MICROS_PER_DAY)),
            ),
            Self::TimeTz {
                micros,
                offset_minutes,
            } => (
                2,
                (i128::from(*micros)
                    - i128::from(*offset_minutes) * 60 * i128::from(MICROS_PER_SECOND))
                .rem_euclid(i128::from(MICROS_PER_DAY)),
            ),
            Self::Timestamp { micros } => (3, i128::from(*micros)),
            Self::TimestampTz { micros } => (4, i128::from(*micros)),
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

fn format_time_micros(micros: i64) -> String {
    let normalized = micros.rem_euclid(MICROS_PER_DAY);
    let seconds = normalized / MICROS_PER_SECOND;
    let micros = normalized % MICROS_PER_SECOND;
    let (Ok(seconds), Some(nanos)) = (
        u32::try_from(seconds),
        micros
            .checked_mul(1_000)
            .and_then(|value| u32::try_from(value).ok()),
    ) else {
        return normalized.to_string();
    };
    let Some(time) = NaiveTime::from_num_seconds_from_midnight_opt(seconds, nanos) else {
        return normalized.to_string();
    };
    let mut out = time.format("%H:%M:%S").to_string();
    if micros != 0 {
        let mut frac = format!("{micros:06}");
        while frac.ends_with('0') {
            frac.pop();
        }
        out.push('.');
        out.push_str(&frac);
    }
    out
}

fn format_offset(offset_minutes: i32) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let abs = offset_minutes.abs();
    format!("{sign}{:02}:{:02}", abs / 60, abs % 60)
}

fn format_timestamp_micros(micros: i64, utc: bool) -> String {
    let Some(dt) = DateTime::from_timestamp_micros(micros) else {
        return micros.to_string();
    };
    let mut out = dt.naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();
    let frac = micros.rem_euclid(MICROS_PER_SECOND);
    if frac != 0 {
        let mut text = format!("{frac:06}");
        while text.ends_with('0') {
            text.pop();
        }
        out.push('.');
        out.push_str(&text);
    }
    if utc {
        // PostgreSQL renders timestamptz in the session time zone; the
        // engine pins UTC, which psql shows as a `+00` suffix.
        out.push_str("+00");
    }
    out
}

/// Render an interval the way `PostgreSQL`'s default (`postgres`)
/// `IntervalStyle` does: `1 year 2 mons 3 days 04:05:06`, per-field
/// signs, and an explicit `+` on a positive field that follows a
/// negative one (`-1 days +03:00:00`).
fn format_interval(months: i32, days: i32, micros: i64) -> String {
    use std::fmt::Write as _;
    let years = months / 12;
    let months = months % 12;
    let mut out = String::new();
    let mut is_before = false;
    let push_unit = |out: &mut String, value: i32, unit: &str, is_before: &mut bool| {
        if value == 0 {
            return;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        if *is_before && value > 0 {
            out.push('+');
        }
        out.push_str(&value.to_string());
        out.push(' ');
        out.push_str(unit);
        if value != 1 {
            out.push('s');
        }
        *is_before = *is_before || value < 0;
    };
    push_unit(&mut out, years, "year", &mut is_before);
    push_unit(&mut out, months, "mon", &mut is_before);
    push_unit(&mut out, days, "day", &mut is_before);
    if micros != 0 || out.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        if micros < 0 {
            out.push('-');
        } else if is_before {
            out.push('+');
        }
        let abs = micros.unsigned_abs();
        let hours = abs / 3_600_000_000;
        let minutes = (abs % 3_600_000_000) / 60_000_000;
        let seconds = (abs % 60_000_000) / 1_000_000;
        let frac = abs % 1_000_000;
        let _ = write!(out, "{hours:02}:{minutes:02}:{seconds:02}");
        if frac != 0 {
            let mut text = format!("{frac:06}");
            while text.ends_with('0') {
                text.pop();
            }
            out.push('.');
            out.push_str(&text);
        }
    }
    out
}

/// Parse a `PostgreSQL` interval literal into `(months, days, micros)`.
#[allow(clippy::too_many_lines)]
fn parse_interval_literal(input: &str) -> Option<TemporalValue> {
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

    let mut text = input.trim().to_ascii_lowercase();
    let mut negate_all = false;
    if let Some(stripped) = text.strip_suffix("ago") {
        negate_all = true;
        text = stripped.trim_end().to_string();
    }
    if text.is_empty() {
        return None;
    }
    let mut acc = Acc::default();
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut pending: Option<f64> = None;
    for token in &tokens {
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
    let parts: Vec<&str> = body.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let hours: i64 = parts[0].parse().ok()?;
    let minutes: i64 = parts[1].parse().ok()?;
    if !(0..60).contains(&minutes) {
        return None;
    }
    let mut micros = hours
        .checked_mul(3_600)?
        .checked_mul(MICROS_PER_SECOND)?
        .checked_add(minutes.checked_mul(60)?.checked_mul(MICROS_PER_SECOND)?)?;
    if parts.len() == 3 {
        let seconds: f64 = parts[2].parse().ok()?;
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
