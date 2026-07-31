//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core value types for UQA: doc ids, payloads, posting entries, and the
//! dynamic [`Value`] used inside payload fields.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::str::FromStr;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Document identifier.
///
/// `u64` addresses up to ~1.8e19 documents while keeping the on-disk
/// representation compact at 8 bytes per posting entry head.
pub type DocId = u64;

/// Field name within a document.
pub type FieldName = String;

/// One step of a hierarchical-document path. Mirrors the canonical UQA implementation's
/// `PathExpr = list[str | int]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

/// A path expression - a sequence of [`PathSegment`]s navigating a
/// hierarchical document.
pub type PathExpr = Vec<PathSegment>;

const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;

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
        let date = NaiveDate::parse_from_str(input.trim(), "%Y-%m-%d").ok()?;
        let days = date.signed_duration_since(epoch_date()).num_days();
        Some(Self::Date {
            days: i32::try_from(days).ok()?,
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

/// Exact base-10 numeric value for `PostgreSQL` `NUMERIC` / `DECIMAL`.
///
/// The JSON representation is tagged so persisted document values do not
/// collide with ordinary JSON strings, numbers, or maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecimalValue {
    inner: Decimal,
}

impl DecimalValue {
    pub fn new(inner: Decimal) -> Self {
        Self { inner }
    }

    pub fn parse(input: &str) -> Option<Self> {
        Decimal::from_str(input.trim()).ok().map(Self::new)
    }

    pub fn from_i64(value: i64) -> Self {
        Self::new(Decimal::from(value))
    }

    pub fn from_bool(value: bool) -> Self {
        Self::from_i64(i64::from(value))
    }

    pub fn from_f64_lossy(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        let parsed = Self::parse(&value.to_string())?;
        if value != 0.0 && parsed.is_zero() {
            return None;
        }
        Some(parsed)
    }

    pub fn is_zero(&self) -> bool {
        self.inner.is_zero()
    }

    pub fn checked_add(&self, rhs: &Self) -> Option<Self> {
        self.inner.checked_add(rhs.inner).map(Self::new)
    }

    pub fn checked_sub(&self, rhs: &Self) -> Option<Self> {
        self.inner.checked_sub(rhs.inner).map(Self::new)
    }

    pub fn checked_mul(&self, rhs: &Self) -> Option<Self> {
        self.inner.checked_mul(rhs.inner).map(Self::new)
    }

    pub fn checked_div(&self, rhs: &Self) -> Option<Self> {
        self.inner.checked_div(rhs.inner).map(Self::new)
    }

    pub fn checked_rem(&self, rhs: &Self) -> Option<Self> {
        self.inner.checked_rem(rhs.inner).map(Self::new)
    }

    pub fn abs(&self) -> Self {
        if self.inner < Decimal::from(0) {
            Self::new(-self.inner)
        } else {
            self.clone()
        }
    }

    pub fn ceil(&self) -> Self {
        Self::new(self.inner.ceil())
    }

    pub fn floor(&self) -> Self {
        Self::new(self.inner.floor())
    }

    pub fn trunc(&self) -> Self {
        Self::new(self.inner.trunc())
    }

    pub fn round_dp(&self, scale: u32) -> Self {
        Self::new(
            self.inner
                .round_dp_with_strategy(scale, RoundingStrategy::MidpointAwayFromZero),
        )
    }

    pub fn round_to_scale(&self, scale: i32) -> Option<Self> {
        if scale >= 0 {
            return Some(self.round_dp(u32::try_from(scale).ok()?));
        }
        let factor = decimal_pow10(scale.unsigned_abs())?;
        self.inner
            .checked_div(factor)
            .map(|value| value.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero))
            .and_then(|value| value.checked_mul(factor))
            .map(Self::new)
    }

    pub fn trunc_to_scale(&self, scale: i32) -> Option<Self> {
        if scale >= 0 {
            return Some(Self::new(
                self.inner.trunc_with_scale(u32::try_from(scale).ok()?),
            ));
        }
        let factor = decimal_pow10(scale.unsigned_abs())?;
        self.inner
            .checked_div(factor)
            .map(|value| value.trunc())
            .and_then(|value| value.checked_mul(factor))
            .map(Self::new)
    }

    pub fn fits_precision(&self, precision: u32, scale: i32) -> bool {
        let Some(factor) = decimal_pow10(scale.unsigned_abs()) else {
            return false;
        };
        let scaled = if scale >= 0 {
            self.inner.checked_mul(factor)
        } else {
            self.inner.checked_div(factor)
        };
        let Some(scaled) = scaled else {
            return false;
        };
        let Ok(precision) = usize::try_from(precision) else {
            return false;
        };
        decimal_integer_digit_count(scaled) <= precision
    }

    pub fn to_sql_string(&self) -> String {
        self.inner.to_string()
    }

    pub fn to_canonical_string(&self) -> String {
        self.inner.normalize().to_string()
    }

    pub fn to_i64_trunc(&self) -> Option<i64> {
        self.inner.trunc().to_string().parse::<i64>().ok()
    }

    pub fn to_f64(&self) -> Option<f64> {
        self.inner.to_string().parse::<f64>().ok()
    }
}

fn decimal_pow10(power: u32) -> Option<Decimal> {
    let mut value = Decimal::from(1);
    for _ in 0..power {
        value = value.checked_mul(Decimal::from(10))?;
    }
    Some(value)
}

fn decimal_integer_digit_count(value: Decimal) -> usize {
    let text = value.abs().trunc().normalize().to_string();
    let digits = text.trim_start_matches('0');
    digits.len().max(1)
}

impl PartialOrd for DecimalValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DecimalValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.inner.cmp(&other.inner)
    }
}

impl Serialize for DecimalValue {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct TaggedDecimal<'a> {
            #[serde(rename = "$uqa_type")]
            kind: &'a str,
            value: String,
        }

        TaggedDecimal {
            kind: "decimal",
            value: self.to_sql_string(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DecimalValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct TaggedDecimal {
            #[serde(rename = "$uqa_type")]
            kind: String,
            value: String,
        }

        let tagged = TaggedDecimal::deserialize(deserializer)?;
        if tagged.kind != "decimal" {
            return Err(serde::de::Error::custom("not a decimal value"));
        }
        Self::parse(&tagged.value).ok_or_else(|| serde::de::Error::custom("invalid decimal value"))
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

/// Dynamic value type for document fields and posting payload extras.
///
/// Covers the JSON-like values the engine round-trips through a posting
/// list. Date and datetime variants land with the SQL type system.
#[derive(Debug, Clone, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Temporal(TemporalValue),
    Decimal(DecimalValue),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

/// Field used by the graph posting-list Phi encoding.
///
/// This is public only so `uqa-graph` and the core posting merge logic can
/// share one versioned codec. Applications should treat it as an opaque
/// implementation detail.
#[doc(hidden)]
pub const GRAPH_PHI_FIELD: &str = "_uqa_graph_phi";
#[doc(hidden)]
pub const GRAPH_PHI_VERTICES_FIELD: &str = "_graph_vertices";
#[doc(hidden)]
pub const GRAPH_PHI_EDGES_FIELD: &str = "_graph_edges";

const GRAPH_PHI_MAGIC: &str = "uqa.graph.phi";
const GRAPH_PHI_VERSION: i64 = 1;
const GRAPH_PHI_MAGIC_KEY: &str = "magic";
const GRAPH_PHI_VERSION_KEY: &str = "version";
const GRAPH_PHI_BASE_SCORE_KEY: &str = "base_score";
const GRAPH_PHI_GRAPH_PRESENT_KEY: &str = "graph_present";
const GRAPH_PHI_VERTICES_KEY: &str = "vertices";
const GRAPH_PHI_EDGES_KEY: &str = "edges";
const GRAPH_PHI_GRAPH_NAME_KEY: &str = "graph_name";
const GRAPH_PHI_OVERRIDE_PRESENT_KEY: &str = "override_present";
const GRAPH_PHI_OVERRIDE_SCORE_KEY: &str = "override_score";
const GRAPH_PHI_ORIGINAL_PRESENT_KEY: &str = "original_present";
const GRAPH_PHI_ORIGINAL_VALUE_KEY: &str = "original_value";
const GRAPH_PHI_ORIGINAL_VERTICES_PRESENT_KEY: &str = "original_vertices_present";
const GRAPH_PHI_ORIGINAL_VERTICES_VALUE_KEY: &str = "original_vertices_value";
const GRAPH_PHI_ORIGINAL_EDGES_PRESENT_KEY: &str = "original_edges_present";
const GRAPH_PHI_ORIGINAL_EDGES_VALUE_KEY: &str = "original_edges_value";
const GRAPH_PHI_FIELD_COUNT: usize = 15;

/// Graph-specific part of a versioned Phi envelope.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphPhiPayload {
    pub vertices: Vec<VertexId>,
    pub edges: Vec<EdgeId>,
    pub graph_name: String,
}

impl GraphPhiPayload {
    #[doc(hidden)]
    pub fn encoded_vertices(&self) -> Value {
        encode_u64_list(&self.vertices)
    }

    #[doc(hidden)]
    pub fn encoded_edges(&self) -> Value {
        encode_u64_list(&self.edges)
    }
}

/// Lossless metadata carried through ordinary posting payload merges.
///
/// Scores use their IEEE-754 bit representation in [`Value`] so `-0.0` and
/// NaN payloads survive a non-colliding round trip exactly. The original value
/// at [`GRAPH_PHI_FIELD`] is nested in the envelope, making the reserved field
/// collision-safe for values produced by the encoder.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct GraphPhiEnvelope {
    pub base_score: f64,
    pub graph_payload: Option<GraphPhiPayload>,
    pub score_override: Option<f64>,
    pub original_reserved: Option<Value>,
    pub original_vertices: Option<Value>,
    pub original_edges: Option<Value>,
}

impl GraphPhiEnvelope {
    #[doc(hidden)]
    pub fn encode(self) -> Value {
        let (graph_present, vertices, edges, graph_name) = self.graph_payload.map_or_else(
            || (false, Vec::new(), Vec::new(), String::new()),
            |graph| (true, graph.vertices, graph.edges, graph.graph_name),
        );
        let (override_present, override_score) = self.score_override.map_or_else(
            || (false, Value::Null),
            |score| (true, encode_f64_bits(score)),
        );
        let (original_present, original_value) = self
            .original_reserved
            .map_or_else(|| (false, Value::Null), |value| (true, value));
        let (original_vertices_present, original_vertices_value) = self
            .original_vertices
            .map_or_else(|| (false, Value::Null), |value| (true, value));
        let (original_edges_present, original_edges_value) = self
            .original_edges
            .map_or_else(|| (false, Value::Null), |value| (true, value));

        Value::Map(BTreeMap::from([
            (
                GRAPH_PHI_MAGIC_KEY.to_string(),
                Value::Str(GRAPH_PHI_MAGIC.to_string()),
            ),
            (
                GRAPH_PHI_VERSION_KEY.to_string(),
                Value::Int(GRAPH_PHI_VERSION),
            ),
            (
                GRAPH_PHI_BASE_SCORE_KEY.to_string(),
                encode_f64_bits(self.base_score),
            ),
            (
                GRAPH_PHI_GRAPH_PRESENT_KEY.to_string(),
                Value::Bool(graph_present),
            ),
            (
                GRAPH_PHI_VERTICES_KEY.to_string(),
                encode_u64_list(&vertices),
            ),
            (GRAPH_PHI_EDGES_KEY.to_string(), encode_u64_list(&edges)),
            (GRAPH_PHI_GRAPH_NAME_KEY.to_string(), Value::Str(graph_name)),
            (
                GRAPH_PHI_OVERRIDE_PRESENT_KEY.to_string(),
                Value::Bool(override_present),
            ),
            (GRAPH_PHI_OVERRIDE_SCORE_KEY.to_string(), override_score),
            (
                GRAPH_PHI_ORIGINAL_PRESENT_KEY.to_string(),
                Value::Bool(original_present),
            ),
            (GRAPH_PHI_ORIGINAL_VALUE_KEY.to_string(), original_value),
            (
                GRAPH_PHI_ORIGINAL_VERTICES_PRESENT_KEY.to_string(),
                Value::Bool(original_vertices_present),
            ),
            (
                GRAPH_PHI_ORIGINAL_VERTICES_VALUE_KEY.to_string(),
                original_vertices_value,
            ),
            (
                GRAPH_PHI_ORIGINAL_EDGES_PRESENT_KEY.to_string(),
                Value::Bool(original_edges_present),
            ),
            (
                GRAPH_PHI_ORIGINAL_EDGES_VALUE_KEY.to_string(),
                original_edges_value,
            ),
        ]))
    }

    /// Decode only the exact current schema. A lookalike or future version is
    /// an ordinary application field, not a partially decoded envelope.
    #[doc(hidden)]
    pub fn decode(value: Option<&Value>) -> Option<Self> {
        let Value::Map(fields) = value? else {
            return None;
        };
        let valid_magic = matches!(
            fields.get(GRAPH_PHI_MAGIC_KEY),
            Some(Value::Str(magic)) if magic == GRAPH_PHI_MAGIC
        );
        if fields.len() != GRAPH_PHI_FIELD_COUNT
            || !valid_magic
            || fields.get(GRAPH_PHI_VERSION_KEY) != Some(&Value::Int(GRAPH_PHI_VERSION))
        {
            return None;
        }

        let base_score = decode_f64_bits(fields.get(GRAPH_PHI_BASE_SCORE_KEY)?)?;
        let Value::Bool(graph_present) = fields.get(GRAPH_PHI_GRAPH_PRESENT_KEY)? else {
            return None;
        };
        let vertices = decode_u64_list(fields.get(GRAPH_PHI_VERTICES_KEY)?)?;
        let edges = decode_u64_list(fields.get(GRAPH_PHI_EDGES_KEY)?)?;
        let Value::Str(graph_name) = fields.get(GRAPH_PHI_GRAPH_NAME_KEY)? else {
            return None;
        };
        let Value::Bool(override_present) = fields.get(GRAPH_PHI_OVERRIDE_PRESENT_KEY)? else {
            return None;
        };
        let override_value = fields.get(GRAPH_PHI_OVERRIDE_SCORE_KEY)?;
        let score_override = match (*override_present, override_value) {
            (true, value) => Some(decode_f64_bits(value)?),
            (false, Value::Null) => None,
            (false, _) => return None,
        };
        let Value::Bool(original_present) = fields.get(GRAPH_PHI_ORIGINAL_PRESENT_KEY)? else {
            return None;
        };
        let original_value = fields.get(GRAPH_PHI_ORIGINAL_VALUE_KEY)?;
        let original_reserved = match (*original_present, original_value) {
            (true, value) => Some(value.clone()),
            (false, Value::Null) => None,
            (false, _) => return None,
        };
        let original_vertices = decode_optional_shadow(
            fields.get(GRAPH_PHI_ORIGINAL_VERTICES_PRESENT_KEY)?,
            fields.get(GRAPH_PHI_ORIGINAL_VERTICES_VALUE_KEY)?,
        )
        .ok()?;
        let original_edges = decode_optional_shadow(
            fields.get(GRAPH_PHI_ORIGINAL_EDGES_PRESENT_KEY)?,
            fields.get(GRAPH_PHI_ORIGINAL_EDGES_VALUE_KEY)?,
        )
        .ok()?;

        let graph_payload = if *graph_present {
            Some(GraphPhiPayload {
                vertices,
                edges,
                graph_name: graph_name.clone(),
            })
        } else {
            if !vertices.is_empty()
                || !edges.is_empty()
                || !graph_name.is_empty()
                || score_override.is_some()
            {
                return None;
            }
            None
        };

        Some(Self {
            base_score,
            graph_payload,
            score_override,
            original_reserved,
            original_vertices,
            original_edges,
        })
    }

    /// Whether a value claims the Phi namespace, even if its schema or
    /// version is unsupported. Such values must not fall through to the
    /// ambiguous legacy two-field decoder.
    #[doc(hidden)]
    pub fn is_recognized(value: Option<&Value>) -> bool {
        matches!(
            value,
            Some(Value::Map(fields))
                if matches!(
                    fields.get(GRAPH_PHI_MAGIC_KEY),
                    Some(Value::Str(magic)) if magic == GRAPH_PHI_MAGIC
                )
        )
    }
}

#[derive(Debug)]
struct InvalidGraphPhiEnvelope;

fn decode_optional_shadow(
    present: &Value,
    value: &Value,
) -> std::result::Result<Option<Value>, InvalidGraphPhiEnvelope> {
    match (present, value) {
        (Value::Bool(true), value) => Ok(Some(value.clone())),
        (Value::Bool(false), Value::Null) => Ok(None),
        _ => Err(InvalidGraphPhiEnvelope),
    }
}

fn encode_f64_bits(value: f64) -> Value {
    Value::Bytes(value.to_bits().to_be_bytes().to_vec())
}

fn decode_f64_bits(value: &Value) -> Option<f64> {
    let Value::Bytes(bytes) = value else {
        return None;
    };
    let encoded: [u8; size_of::<u64>()] = bytes.as_slice().try_into().ok()?;
    Some(f64::from_bits(u64::from_be_bytes(encoded)))
}

fn encode_u64_list(values: &[u64]) -> Value {
    Value::List(
        values
            .iter()
            .map(|value| Value::Bytes(value.to_be_bytes().to_vec()))
            .collect(),
    )
}

fn decode_u64_list(value: &Value) -> Option<Vec<u64>> {
    let Value::List(values) = value else {
        return None;
    };
    values
        .iter()
        .map(|value| {
            let Value::Bytes(bytes) = value else {
                return None;
            };
            let encoded: [u8; size_of::<u64>()] = bytes.as_slice().try_into().ok()?;
            Some(u64::from_be_bytes(encoded))
        })
        .collect()
}

impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::Int(value) => serializer.serialize_i64(*value),
            Self::Float(value) => serializer.serialize_f64(*value),
            Self::Str(value) => serializer.serialize_str(value),
            Self::Bytes(value) => {
                const DIGITS: &[u8; 16] = b"0123456789abcdef";

                #[derive(Serialize)]
                struct TaggedBytes<'a> {
                    #[serde(rename = "$uqa_type")]
                    kind: &'static str,
                    hex: &'a str,
                }

                let capacity = value.len().checked_mul(2).ok_or_else(|| {
                    <S::Error as serde::ser::Error>::custom(
                        "byte value hex representation exceeds the addressable range",
                    )
                })?;
                let mut hex = String::new();
                hex.try_reserve_exact(capacity).map_err(|error| {
                    <S::Error as serde::ser::Error>::custom(format!(
                        "cannot allocate byte value hex representation: {error}"
                    ))
                })?;
                for byte in value {
                    hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
                    hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
                }
                TaggedBytes {
                    kind: "bytes",
                    hex: &hex,
                }
                .serialize(serializer)
            }
            Self::Temporal(value) => value.serialize(serializer),
            Self::Decimal(value) => value.serialize(serializer),
            Self::List(value) => value.serialize(serializer),
            Self::Map(value) => value.serialize(serializer),
        }
    }
}

/// Reconstruct the value a `$uqa_type`-tagged map encodes, or `None`
/// when the map does not match any tagged encoding and must stay a
/// plain [`Value::Map`].
///
/// Temporal variants mirror the `deny_unknown_fields` internally-tagged
/// derive on [`TemporalValue`]: the field set must match exactly and
/// every field must be an in-range integer. The decimal encoding
/// mirrors the tolerant tagged struct in [`DecimalValue`]'s
/// `Deserialize`: extra fields are ignored.
fn value_from_tagged_map(
    tag: &str,
    map: &BTreeMap<String, Value>,
) -> Result<Option<Value>, String> {
    fn int_field<T: TryFrom<i64>>(map: &BTreeMap<String, Value>, key: &str) -> Option<T> {
        match map.get(key)? {
            Value::Int(number) => T::try_from(*number).ok(),
            _ => None,
        }
    }

    let temporal = match tag {
        "date" if map.len() == 2 => {
            let Some(days) = int_field(map, "days") else {
                return Ok(None);
            };
            TemporalValue::Date { days }
        }
        "time" if map.len() == 2 => {
            let Some(micros) = int_field(map, "micros") else {
                return Ok(None);
            };
            TemporalValue::Time { micros }
        }
        "time_tz" if map.len() == 3 => {
            let (Some(micros), Some(offset_minutes)) =
                (int_field(map, "micros"), int_field(map, "offset_minutes"))
            else {
                return Ok(None);
            };
            TemporalValue::TimeTz {
                micros,
                offset_minutes,
            }
        }
        "timestamp" if map.len() == 2 => {
            let Some(micros) = int_field(map, "micros") else {
                return Ok(None);
            };
            TemporalValue::Timestamp { micros }
        }
        "timestamp_tz" if map.len() == 2 => {
            let Some(micros) = int_field(map, "micros") else {
                return Ok(None);
            };
            TemporalValue::TimestampTz { micros }
        }
        "interval" if map.len() == 4 => {
            let (Some(months), Some(days), Some(micros)) = (
                int_field(map, "months"),
                int_field(map, "days"),
                int_field(map, "micros"),
            ) else {
                return Ok(None);
            };
            TemporalValue::Interval {
                months,
                days,
                micros,
            }
        }
        "decimal" => {
            let Some(Value::Str(text)) = map.get("value") else {
                return Ok(None);
            };
            return Ok(DecimalValue::parse(text).map(Value::Decimal));
        }
        "bytes" if map.len() == 2 => {
            let Some(Value::Str(hex)) = map.get("hex") else {
                return Ok(None);
            };
            return decode_hex_bytes(hex).map(|bytes| bytes.map(Value::Bytes));
        }
        _ => return Ok(None),
    };
    Ok(Some(Value::Temporal(temporal)))
}

fn decode_hex_bytes(hex: &str) -> Result<Option<Vec<u8>>, String> {
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let encoded = hex.as_bytes();
    if encoded.len() % 2 != 0 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(encoded.len() / 2)
        .map_err(|error| format!("cannot allocate decoded byte value: {error}"))?;
    for pair in encoded.chunks_exact(2) {
        let (Some(high), Some(low)) = (nibble(pair[0]), nibble(pair[1])) else {
            return Ok(None);
        };
        bytes.push((high << 4) | low);
    }
    Ok(Some(bytes))
}

/// Hand-written [`Deserialize`] for scalar JSON values, explicit tagged
/// byte/temporal/decimal values, ordinary arrays, and maps, without the untagged
/// machinery's per-variant trial errors. Untagged deserialization
/// buffers the input and formats a rejection error for every variant
/// that does not match; profiling showed that error construction alone
/// consuming a quarter of `SQLite` read time. The visitor dispatches on
/// the self-describing input directly instead.
impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ValueVisitor;

        impl<'de> serde::de::Visitor<'de> for ValueVisitor {
            type Value = Value;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a UQA value")
            }

            fn visit_unit<E>(self) -> std::result::Result<Value, E> {
                Ok(Value::Null)
            }

            fn visit_none<E>(self) -> std::result::Result<Value, E> {
                Ok(Value::Null)
            }

            fn visit_some<D>(self, deserializer: D) -> std::result::Result<Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                Value::deserialize(deserializer)
            }

            fn visit_bool<E>(self, value: bool) -> std::result::Result<Value, E> {
                Ok(Value::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<Value, E> {
                Ok(Value::Int(value))
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Value, E> {
                // The untagged order tries Int(i64) before Float, so
                // only out-of-range magnitudes land on Float.
                Ok(i64::try_from(value).map_or(Value::Float(value as f64), Value::Int))
            }

            fn visit_f64<E>(self, value: f64) -> std::result::Result<Value, E> {
                Ok(Value::Float(value))
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Value, E> {
                Ok(Value::Str(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Value, E> {
                Ok(Value::Str(value))
            }

            fn visit_bytes<E>(self, value: &[u8]) -> std::result::Result<Value, E> {
                Ok(Value::Bytes(value.to_vec()))
            }

            fn visit_byte_buf<E>(self, value: Vec<u8>) -> std::result::Result<Value, E> {
                Ok(Value::Bytes(value))
            }

            fn visit_seq<A>(self, mut access: A) -> std::result::Result<Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                // `SeqAccess::size_hint` is supplied by the input decoder and
                // is not trustworthy enough to hand directly to an infallible
                // allocation. Reserve a bounded useful prefix, then make every
                // subsequent growth fallible as elements actually arrive.
                let initial = access.size_hint().unwrap_or(0).min(4_096);
                let mut items: Vec<Value> = Vec::new();
                items.try_reserve_exact(initial).map_err(|error| {
                    <A::Error as serde::de::Error>::custom(format!(
                        "cannot allocate UQA value sequence: {error}"
                    ))
                })?;
                while let Some(item) = access.next_element::<Value>()? {
                    if items.len() == items.capacity() {
                        items.try_reserve(1).map_err(|error| {
                            <A::Error as serde::de::Error>::custom(format!(
                                "cannot grow UQA value sequence: {error}"
                            ))
                        })?;
                    }
                    items.push(item);
                }
                Ok(Value::List(items))
            }

            fn visit_map<A>(self, mut access: A) -> std::result::Result<Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut map = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, Value>()? {
                    map.insert(key, value);
                }
                if let Some(Value::Str(tag)) = map.get("$uqa_type") {
                    if let Some(value) = value_from_tagged_map(tag, &map)
                        .map_err(<A::Error as serde::de::Error>::custom)?
                    {
                        return Ok(value);
                    }
                }
                Ok(Value::Map(map))
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

/// Posting list entry payload: token positions, relevance score, and any
/// extra field values the operator pipeline carries forward.
///
/// `positions` is sorted ascending with no duplicates. `fields` uses
/// `BTreeMap` (not `HashMap`) so equality and iteration are deterministic
/// across storage, merge, and regression tests.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Payload {
    pub positions: Vec<u32>,
    pub score: f64,
    pub fields: BTreeMap<FieldName, Value>,
}

impl Payload {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_score(score: f64) -> Self {
        Self {
            score,
            ..Self::default()
        }
    }
}

/// A single `(doc_id, payload)` entry in a posting list.
#[derive(Debug, Clone, PartialEq)]
pub struct PostingEntry {
    pub doc_id: DocId,
    pub payload: Payload,
}

impl PostingEntry {
    pub fn new(doc_id: DocId, payload: Payload) -> Self {
        Self { doc_id, payload }
    }
}

/// Join result entry with multi-document tuples (Definition 4.1.2, Paper 1).
///
/// `doc_ids` is ordered the same way as the joined relations contributed to
/// the result; equality and ordering are tuple-wise.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GeneralizedPostingEntry {
    pub doc_ids: Vec<DocId>,
    pub payload: GeneralizedPayload,
}

/// Payload for `GeneralizedPostingEntry`. Carries no floating-point
/// score, so `Eq`/`Ord` derive cleanly and joined entries can key directly
/// off `(doc_ids, payload)` without a separate ordering helper.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct GeneralizedPayload {
    pub fields: BTreeMap<FieldName, Value>,
}

/// Stable identifier for a graph vertex. `u64` keeps adjacency entries
/// compact and fits up to 1.8e19 vertices per graph store.
pub type VertexId = u64;

/// Stable identifier for a graph edge.
pub type EdgeId = u64;

/// Property graph vertex: `(id, label, properties)`. Properties are typed
/// by [`Value`] so vertex props share the same encoding as document fields.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Vertex {
    pub vertex_id: VertexId,
    pub label: String,
    pub properties: BTreeMap<String, Value>,
}

impl Vertex {
    pub fn new(vertex_id: VertexId, label: impl Into<String>) -> Self {
        Self {
            vertex_id,
            label: label.into(),
            properties: BTreeMap::new(),
        }
    }
}

/// Directed property graph edge: `(id, source, target, label, properties)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub edge_id: EdgeId,
    pub source_id: VertexId,
    pub target_id: VertexId,
    pub label: String,
    pub properties: BTreeMap<String, Value>,
}

impl Edge {
    pub fn new(
        edge_id: EdgeId,
        source_id: VertexId,
        target_id: VertexId,
        label: impl Into<String>,
    ) -> Self {
        Self {
            edge_id,
            source_id,
            target_id,
            label: label.into(),
            properties: BTreeMap::new(),
        }
    }
}

/// Index-level statistics consumed by the cost model and BM25 scorer.
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub total_docs: u64,
    pub avg_doc_length: f64,
    pub dimensions: u32,
    doc_freqs: BTreeMap<(FieldName, String), u64>,
}

impl IndexStats {
    /// Build a new [`IndexStats`] with the given total document count
    /// and an empty frequency table.
    pub fn new(total_docs: u64) -> Self {
        Self {
            total_docs,
            avg_doc_length: 0.0,
            dimensions: 0,
            doc_freqs: BTreeMap::new(),
        }
    }

    pub fn doc_freq(&self, field: &str, term: &str) -> u64 {
        self.doc_freqs
            .get(&(field.to_string(), term.to_string()))
            .copied()
            .unwrap_or(0)
    }

    pub fn set_doc_freq(&mut self, field: impl Into<FieldName>, term: impl Into<String>, df: u64) {
        self.doc_freqs.insert((field.into(), term.into()), df);
    }

    /// Builder-style insert that returns the modified [`IndexStats`].
    pub fn with_doc_freq(
        mut self,
        field: impl Into<FieldName>,
        term: impl Into<String>,
        df: u64,
    ) -> Self {
        self.set_doc_freq(field, term, df);
        self
    }
}

fn compare_floats(left: f64, right: f64) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if left == right {
        return Ordering::Equal;
    }
    if left.is_nan() {
        return if right.is_nan() {
            Ordering::Equal
        } else {
            Ordering::Greater
        };
    }
    if right.is_nan() || left < right {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

fn compare_integer_float(integer: i64, float: f64) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    const I64_UPPER_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    const I64_LOWER_INCLUSIVE: f64 = -9_223_372_036_854_775_808.0;

    if float.is_nan() || float >= I64_UPPER_EXCLUSIVE {
        return Ordering::Less;
    }
    if float < I64_LOWER_INCLUSIVE {
        return Ordering::Greater;
    }
    let truncated = float.trunc() as i64;
    match integer.cmp(&truncated) {
        Ordering::Equal if float > truncated as f64 => Ordering::Less,
        Ordering::Equal if float < truncated as f64 => Ordering::Greater,
        ordering => ordering,
    }
}

fn compare_float_decimal(float: f64, decimal: &DecimalValue) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if float.is_nan() || float == f64::INFINITY {
        return Ordering::Greater;
    }
    if float == f64::NEG_INFINITY {
        return Ordering::Less;
    }
    if let Some(float_decimal) = DecimalValue::from_f64_lossy(float) {
        return float_decimal.cmp(decimal);
    }

    // A finite f64 that rust_decimal cannot represent is either outside its
    // magnitude or below its scale. Compare such subnormal/supernormal values
    // against zero and the decimal sign without manufacturing an equality.
    let decimal_vs_zero = decimal.cmp(&DecimalValue::from_i64(0));
    if float > 0.0 {
        if float > 1.0 {
            Ordering::Greater
        } else if decimal_vs_zero == Ordering::Greater {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    } else if float < -1.0 {
        Ordering::Less
    } else if decimal_vs_zero == Ordering::Less {
        Ordering::Greater
    } else {
        Ordering::Less
    }
}

// `Value` carries `f64`, so equality and ordering are implemented together.
// NaN compares equal to NaN and greater than finite values, signed zeroes are
// equal, and cross-numeric variants use numeric rather than discriminant order.
// This keeps `Eq`/`Ord` consistent for BTree keys used by joins and DISTINCT.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => compare_floats(*a, *b),
            (Value::Decimal(a), Value::Decimal(b)) => a.cmp(b),
            // Numeric cross-type compare: Int / Float / Bool all coerce
            // to f64 so SQL `WHERE price > 15` (Float vs Int literal)
            // and `WHERE flag > 0` line up with PostgreSQL semantics
            // instead of falling through to the discriminant order.
            (Value::Int(a), Value::Float(b)) => compare_integer_float(*a, *b),
            (Value::Float(a), Value::Int(b)) => compare_integer_float(*b, *a).reverse(),
            (Value::Int(a), Value::Decimal(b)) => DecimalValue::from_i64(*a).cmp(b),
            (Value::Decimal(a), Value::Int(b)) => a.cmp(&DecimalValue::from_i64(*b)),
            (Value::Float(a), Value::Decimal(b)) => compare_float_decimal(*a, b),
            (Value::Decimal(a), Value::Float(b)) => compare_float_decimal(*b, a).reverse(),
            (Value::Bool(a), Value::Int(b)) => i64::from(*a).cmp(b),
            (Value::Int(a), Value::Bool(b)) => a.cmp(&i64::from(*b)),
            (Value::Bool(a), Value::Float(b)) => compare_integer_float(i64::from(*a), *b),
            (Value::Float(a), Value::Bool(b)) => compare_integer_float(i64::from(*b), *a).reverse(),
            (Value::Bool(a), Value::Decimal(b)) => DecimalValue::from_bool(*a).cmp(b),
            (Value::Decimal(a), Value::Bool(b)) => a.cmp(&DecimalValue::from_bool(*b)),
            (Value::Str(a), Value::Str(b)) => a.cmp(b),
            (Value::Bytes(a), Value::Bytes(b)) => a.cmp(b),
            (Value::Temporal(a), Value::Temporal(b)) => a.cmp(b),
            (Value::List(a), Value::List(b)) => a.cmp(b),
            (Value::Map(a), Value::Map(b)) => a.cmp(b),
            _ => discriminant(self).cmp(&discriminant(other)),
        }
    }
}

fn discriminant(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Decimal(_) => 1,
        Value::Str(_) => 2,
        Value::Bytes(_) => 3,
        Value::Temporal(_) => 4,
        Value::List(_) => 5,
        Value::Map(_) => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HostileEmptySequence;

    impl<'de> serde::de::SeqAccess<'de> for HostileEmptySequence {
        type Error = serde::de::value::Error;

        fn next_element_seed<T>(
            &mut self,
            _seed: T,
        ) -> std::result::Result<Option<T::Value>, Self::Error>
        where
            T: serde::de::DeserializeSeed<'de>,
        {
            Ok(None)
        }

        fn size_hint(&self) -> Option<usize> {
            Some(usize::MAX)
        }
    }

    #[test]
    fn value_deserializer_does_not_trust_sequence_size_hints() {
        let decoder = serde::de::value::SeqAccessDeserializer::new(HostileEmptySequence);
        let value = Value::deserialize(decoder).expect("bounded sequence preallocation");
        assert_eq!(value, Value::List(Vec::new()));
    }

    #[test]
    fn payload_default_is_zero_score_and_empty() {
        let p = Payload::default();
        assert_eq!(p.score, 0.0);
        assert!(p.positions.is_empty());
        assert!(p.fields.is_empty());
    }

    #[test]
    fn posting_entry_construction_round_trips() {
        let e = PostingEntry::new(42, Payload::with_score(1.5));
        assert_eq!(e.doc_id, 42);
        let diff: f64 = e.payload.score - 1.5;
        assert!(diff.abs() < f64::EPSILON);
    }

    #[test]
    fn index_stats_doc_freq_default_zero() {
        let s = IndexStats::default();
        assert_eq!(s.doc_freq("title", "rust"), 0);
    }

    #[test]
    fn index_stats_records_doc_freq() {
        let mut s = IndexStats::default();
        s.set_doc_freq("title", "rust", 12);
        assert_eq!(s.doc_freq("title", "rust"), 12);
        assert_eq!(s.doc_freq("title", "java"), 0);
    }

    #[test]
    fn generalized_entry_orders_lexicographically() {
        let a = GeneralizedPostingEntry {
            doc_ids: vec![1, 2],
            payload: GeneralizedPayload::default(),
        };
        let b = GeneralizedPostingEntry {
            doc_ids: vec![1, 3],
            payload: GeneralizedPayload::default(),
        };
        assert!(a < b);
    }

    #[test]
    fn value_ordering_within_variant() {
        assert!(Value::Int(1) < Value::Int(2));
        assert!(Value::Str("a".into()) < Value::Str("b".into()));
    }

    #[test]
    fn value_ordering_across_variants_is_stable() {
        assert!(Value::Null < Value::Bool(false));
        // Numeric coercion: Bool(true) == 1 > Int(0).
        assert!(Value::Bool(true) > Value::Int(0));
        // Float vs Int compares numerically (not by discriminant).
        assert!(Value::Float(10.0) < Value::Int(15));
        assert!(Value::Float(20.0) > Value::Int(15));
        assert!(Value::Decimal(DecimalValue::parse("10.5").unwrap()) > Value::Int(10));
        assert_eq!(
            Value::Decimal(DecimalValue::parse("1.0").unwrap()).cmp(&Value::Int(1)),
            std::cmp::Ordering::Equal
        );

        let equivalent = [
            Value::Bool(true),
            Value::Int(1),
            Value::Float(1.0),
            Value::Decimal(DecimalValue::parse("1.0").unwrap()),
        ];
        let text = Value::Str("numeric boundary".into());
        for left in &equivalent {
            for right in &equivalent {
                assert_eq!(left.cmp(right), std::cmp::Ordering::Equal);
                assert_eq!(left.cmp(&text), right.cmp(&text));
            }
        }
    }

    #[test]
    fn value_numeric_order_is_total_and_does_not_round_large_integers() {
        let rounded_float = Value::Float(9_007_199_254_740_992.0);
        let next_integer = Value::Int(9_007_199_254_740_993);
        assert!(next_integer > rounded_float);
        assert_ne!(next_integer, rounded_float);

        let nan = Value::Float(f64::NAN);
        assert_eq!(nan, Value::Float(f64::NAN));
        assert!(nan > Value::Float(f64::INFINITY));
        assert!(nan > Value::Int(i64::MAX));
        assert_eq!(Value::Float(-0.0), Value::Float(0.0));

        let huge = Value::Float(f64::MAX);
        let decimal = Value::Decimal(DecimalValue::parse("79228162514264337593543950335").unwrap());
        assert!(huge > decimal);
        assert!(Value::Float(f64::MIN_POSITIVE) > Value::Decimal(DecimalValue::from_i64(0)));
    }

    #[test]
    fn temporal_ordering_is_overflow_safe_and_consistent_with_equality() {
        let extreme_interval = TemporalValue::Interval {
            months: i32::MAX,
            days: i32::MAX,
            micros: i64::MAX,
        };
        let smaller_interval = TemporalValue::Interval {
            months: i32::MAX,
            days: i32::MAX,
            micros: i64::MAX - 1,
        };
        assert!(extreme_interval > smaller_interval);

        let utc = TemporalValue::TimeTz {
            micros: 0,
            offset_minutes: 0,
        };
        let same_utc = TemporalValue::TimeTz {
            micros: 60 * MICROS_PER_SECOND,
            offset_minutes: 1,
        };
        assert_eq!(utc.cmp(&same_utc), std::cmp::Ordering::Equal);
        assert_eq!(utc, same_utc);

        let deserialized_extreme = TemporalValue::TimeTz {
            micros: i64::MIN,
            offset_minutes: i32::MAX,
        };
        let _ordering = deserialized_extreme.cmp(&utc);
    }

    #[test]
    fn interval_parser_rejects_non_finite_and_overflowing_components() {
        assert_eq!(
            TemporalValue::parse_interval("1.5 days"),
            Some(TemporalValue::Interval {
                months: 0,
                days: 1,
                micros: MICROS_PER_DAY / 2,
            })
        );
        for invalid in [
            "nan seconds",
            "inf seconds",
            "1e309 seconds",
            "9223372036854775808 microseconds",
            "2562047789 hours",
            "9223372036854 seconds 1000000 microseconds",
            "9223372036854775807:00",
            "768614336404564651-0",
        ] {
            assert_eq!(
                TemporalValue::parse_interval(invalid),
                None,
                "unexpectedly accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn decimal_value_uses_tagged_json() {
        let value = Value::Decimal(DecimalValue::parse("123.4500").unwrap());
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("\"$uqa_type\":\"decimal\""));
        assert!(json.contains("\"value\":\"123.4500\""));
        assert_eq!(serde_json::from_str::<Value>(&json).unwrap(), value);
    }

    fn decode(json: &str) -> Value {
        serde_json::from_str::<Value>(json).expect("decodable JSON value")
    }

    /// Scalar decoding retains the numeric precedence of the original
    /// untagged representation; byte values now use an explicit map tag.
    #[test]
    fn value_json_decoding_scalar_shapes() {
        assert_eq!(decode("null"), Value::Null);
        assert_eq!(decode("true"), Value::Bool(true));
        assert_eq!(decode("false"), Value::Bool(false));
        assert_eq!(decode("42"), Value::Int(42));
        assert_eq!(decode("-42"), Value::Int(-42));
        assert_eq!(decode(&i64::MAX.to_string()), Value::Int(i64::MAX));
        assert_eq!(decode(&i64::MIN.to_string()), Value::Int(i64::MIN));
        // u64 beyond the i64 range falls through Int to Float.
        assert_eq!(decode(&u64::MAX.to_string()), Value::Float(u64::MAX as f64));
        assert_eq!(decode("1.5"), Value::Float(1.5));
        assert_eq!(decode("1.0"), Value::Float(1.0));
        assert_eq!(decode("\"hello\""), Value::Str("hello".into()));
        // Strings resolve as Str even when they look temporal.
        assert_eq!(decode("\"2024-01-01\""), Value::Str("2024-01-01".into()));
    }

    #[test]
    fn value_json_decoding_array_shapes() {
        assert_eq!(decode("[]"), Value::List(Vec::new()));
        assert_eq!(
            decode("[1, 2, 255]"),
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(255)])
        );
        assert_eq!(decode("[0]"), Value::List(vec![Value::Int(0)]));
        assert_eq!(
            decode("[1, 256]"),
            Value::List(vec![Value::Int(1), Value::Int(256)])
        );
        assert_eq!(
            decode("[1, -1]"),
            Value::List(vec![Value::Int(1), Value::Int(-1)])
        );
        assert_eq!(
            decode("[1, 2.0]"),
            Value::List(vec![Value::Int(1), Value::Float(2.0)])
        );
        assert_eq!(
            decode("[\"a\", 1]"),
            Value::List(vec![Value::Str("a".into()), Value::Int(1)])
        );
        assert_eq!(
            decode("[[3]]"),
            Value::List(vec![Value::List(vec![Value::Int(3)])])
        );
    }

    #[test]
    fn byte_values_use_an_explicit_tagged_json_encoding() {
        let value = Value::Bytes(vec![0, 1, 15, 16, 255]);
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, r#"{"$uqa_type":"bytes","hex":"00010f10ff"}"#);
        assert_eq!(decode(&json), value);
        assert_eq!(
            decode(r#"{"$uqa_type":"bytes","hex":"00FF"}"#),
            Value::Bytes(vec![0, 255])
        );
    }

    #[test]
    fn value_json_decoding_tagged_map_shapes() {
        // Temporal variants: internally tagged, deny_unknown_fields.
        assert_eq!(
            decode("{\"$uqa_type\":\"date\",\"days\":19723}"),
            Value::Temporal(TemporalValue::Date { days: 19723 })
        );
        assert_eq!(
            decode("{\"$uqa_type\":\"time\",\"micros\":123}"),
            Value::Temporal(TemporalValue::Time { micros: 123 })
        );
        assert_eq!(
            decode("{\"$uqa_type\":\"time_tz\",\"micros\":5,\"offset_minutes\":-90}"),
            Value::Temporal(TemporalValue::TimeTz {
                micros: 5,
                offset_minutes: -90
            })
        );
        assert_eq!(
            decode("{\"$uqa_type\":\"timestamp\",\"micros\":-7}"),
            Value::Temporal(TemporalValue::Timestamp { micros: -7 })
        );
        assert_eq!(
            decode("{\"$uqa_type\":\"timestamp_tz\",\"micros\":8}"),
            Value::Temporal(TemporalValue::TimestampTz { micros: 8 })
        );
        assert_eq!(
            decode("{\"$uqa_type\":\"interval\",\"months\":1,\"days\":2,\"micros\":3}"),
            Value::Temporal(TemporalValue::Interval {
                months: 1,
                days: 2,
                micros: 3
            })
        );
        // A temporal tag with an unknown extra field fails the
        // deny_unknown_fields temporal decode and lands on Map.
        assert_eq!(
            decode("{\"$uqa_type\":\"date\",\"days\":1,\"extra\":2}"),
            Value::Map(BTreeMap::from([
                ("$uqa_type".to_string(), Value::Str("date".into())),
                ("days".to_string(), Value::Int(1)),
                ("extra".to_string(), Value::Int(2)),
            ]))
        );
        // A temporal tag with a missing field also lands on Map.
        assert_eq!(
            decode("{\"$uqa_type\":\"date\"}"),
            Value::Map(BTreeMap::from([(
                "$uqa_type".to_string(),
                Value::Str("date".into())
            ),]))
        );
        // A temporal tag with an out-of-range field value lands on Map.
        assert_eq!(
            decode("{\"$uqa_type\":\"date\",\"days\":4294967296}"),
            Value::Map(BTreeMap::from([
                ("$uqa_type".to_string(), Value::Str("date".into())),
                ("days".to_string(), Value::Int(4_294_967_296)),
            ]))
        );
        // Decimal: tagged struct without deny_unknown_fields, so extra
        // fields are tolerated.
        assert_eq!(
            decode("{\"$uqa_type\":\"decimal\",\"value\":\"1.50\"}"),
            Value::Decimal(DecimalValue::parse("1.50").unwrap())
        );
        assert_eq!(
            decode("{\"$uqa_type\":\"decimal\",\"value\":\"1.50\",\"extra\":true}"),
            Value::Decimal(DecimalValue::parse("1.50").unwrap())
        );
        // An unparseable decimal payload falls through to Map.
        assert_eq!(
            decode("{\"$uqa_type\":\"decimal\",\"value\":\"not a number\"}"),
            Value::Map(BTreeMap::from([
                ("$uqa_type".to_string(), Value::Str("decimal".into())),
                ("value".to_string(), Value::Str("not a number".into())),
            ]))
        );
        // Unknown tags fall through to Map.
        assert_eq!(
            decode("{\"$uqa_type\":\"mystery\",\"value\":1}"),
            Value::Map(BTreeMap::from([
                ("$uqa_type".to_string(), Value::Str("mystery".into())),
                ("value".to_string(), Value::Int(1)),
            ]))
        );
        // A non-string tag falls through to Map.
        assert_eq!(
            decode("{\"$uqa_type\":7}"),
            Value::Map(BTreeMap::from([("$uqa_type".to_string(), Value::Int(7)),]))
        );
    }

    #[test]
    fn value_json_decoding_plain_maps_and_nesting() {
        assert_eq!(decode("{}"), Value::Map(BTreeMap::new()));
        assert_eq!(
            decode("{\"a\":1,\"b\":\"x\"}"),
            Value::Map(BTreeMap::from([
                ("a".to_string(), Value::Int(1)),
                ("b".to_string(), Value::Str("x".into())),
            ]))
        );
        assert_eq!(
            decode("{\"outer\":{\"$uqa_type\":\"date\",\"days\":3}}"),
            Value::Map(BTreeMap::from([(
                "outer".to_string(),
                Value::Temporal(TemporalValue::Date { days: 3 })
            ),]))
        );
        assert_eq!(
            decode("{\"list\":[\"x\",{\"$uqa_type\":\"decimal\",\"value\":\"2\"}]}"),
            Value::Map(BTreeMap::from([(
                "list".to_string(),
                Value::List(vec![
                    Value::Str("x".into()),
                    Value::Decimal(DecimalValue::parse("2").unwrap()),
                ])
            ),]))
        );
    }

    /// The untagged derive accepted serde's sequence form for
    /// internally-tagged enums and tagged structs: an array whose first
    /// element names (or indexes) a temporal variant - or spells
    /// "decimal" - and whose remaining elements match that variant's
    /// fields deserialized as Temporal / Decimal instead of List.
    /// `[1,-1]` silently became `Time { micros: -1 }`. The visitor
    /// deliberately drops that quirk: arrays that are not byte arrays
    /// are lists, which is also the only round-trip-stable reading.
    #[test]
    fn value_json_decoding_keeps_arrays_as_lists() {
        assert_eq!(
            decode("[1,-1]"),
            Value::List(vec![Value::Int(1), Value::Int(-1)])
        );
        assert_eq!(
            decode("[1,256]"),
            Value::List(vec![Value::Int(1), Value::Int(256)])
        );
        assert_eq!(
            decode("[\"time\",-1]"),
            Value::List(vec![Value::Str("time".into()), Value::Int(-1)])
        );
        assert_eq!(
            decode("[\"decimal\",\"1.5\"]"),
            Value::List(vec![Value::Str("decimal".into()), Value::Str("1.5".into())])
        );
    }

    /// Differential check against the previous implementation for shapes
    /// unaffected by the deliberate ordinary-array/explicit-bytes change.
    #[test]
    fn value_json_decoding_matches_untagged_derive() {
        #[derive(Debug, PartialEq, serde::Deserialize)]
        #[serde(untagged)]
        enum UntaggedValue {
            Null,
            Bool(bool),
            Int(i64),
            Float(f64),
            Str(String),
            Bytes(Vec<u8>),
            Temporal(TemporalValue),
            Decimal(DecimalValue),
            List(Vec<UntaggedValue>),
            Map(BTreeMap<String, UntaggedValue>),
        }

        fn to_value(untagged: UntaggedValue) -> Value {
            match untagged {
                UntaggedValue::Null => Value::Null,
                UntaggedValue::Bool(value) => Value::Bool(value),
                UntaggedValue::Int(value) => Value::Int(value),
                UntaggedValue::Float(value) => Value::Float(value),
                UntaggedValue::Str(value) => Value::Str(value),
                UntaggedValue::Bytes(value) => Value::Bytes(value),
                UntaggedValue::Temporal(value) => Value::Temporal(value),
                UntaggedValue::Decimal(value) => Value::Decimal(value),
                UntaggedValue::List(items) => {
                    Value::List(items.into_iter().map(to_value).collect())
                }
                UntaggedValue::Map(map) => Value::Map(
                    map.into_iter()
                        .map(|(key, value)| (key, to_value(value)))
                        .collect(),
                ),
            }
        }

        let corpus = [
            "null",
            "true",
            "false",
            "0",
            "-1",
            "42",
            "9223372036854775807",
            "-9223372036854775808",
            "18446744073709551615",
            "1.5",
            "1.0",
            "-0.0",
            "1e300",
            "\"\"",
            "\"hello\"",
            "\"2024-01-01\"",
            "\"$uqa_type\"",
            "[1,2.0]",
            "[\"a\"]",
            "[{\"a\":1}]",
            "{}",
            "{\"a\":1}",
            "{\"$uqa_type\":\"date\",\"days\":19723}",
            "{\"$uqa_type\":\"date\",\"days\":1,\"extra\":2}",
            "{\"$uqa_type\":\"date\"}",
            "{\"$uqa_type\":\"date\",\"days\":4294967296}",
            "{\"$uqa_type\":\"date\",\"days\":\"x\"}",
            "{\"$uqa_type\":\"date\",\"days\":1.0}",
            "{\"$uqa_type\":\"time\",\"micros\":123}",
            "{\"$uqa_type\":\"time_tz\",\"micros\":5,\"offset_minutes\":-90}",
            "{\"$uqa_type\":\"time_tz\",\"micros\":5}",
            "{\"$uqa_type\":\"timestamp\",\"micros\":-7}",
            "{\"$uqa_type\":\"timestamp_tz\",\"micros\":8}",
            "{\"$uqa_type\":\"interval\",\"months\":1,\"days\":2,\"micros\":3}",
            "{\"$uqa_type\":\"interval\",\"months\":1,\"days\":2}",
            "{\"$uqa_type\":\"decimal\",\"value\":\"1.50\"}",
            "{\"$uqa_type\":\"decimal\",\"value\":\"1.50\",\"extra\":true}",
            "{\"$uqa_type\":\"decimal\",\"value\":\"not a number\"}",
            "{\"$uqa_type\":\"decimal\",\"value\":7}",
            "{\"$uqa_type\":\"decimal\"}",
            "{\"$uqa_type\":\"mystery\",\"value\":1}",
            "{\"$uqa_type\":7}",
            "{\"$uqa_type\":[\"date\"]}",
            "{\"outer\":{\"$uqa_type\":\"date\",\"days\":3}}",
            "[{\"$uqa_type\":\"decimal\",\"value\":\"2\"},\"x\"]",
        ];
        for json in corpus {
            let expected = to_value(serde_json::from_str::<UntaggedValue>(json).unwrap());
            assert_eq!(
                serde_json::from_str::<Value>(json).unwrap(),
                expected,
                "visitor and untagged derive disagree for {json}"
            );
        }
    }

    #[test]
    fn value_json_round_trips_every_variant() {
        let values = vec![
            Value::Null,
            Value::Bool(true),
            Value::Int(-5),
            Value::Float(2.25),
            Value::Str("text".into()),
            Value::Bytes(vec![0, 1, 255]),
            Value::Temporal(TemporalValue::Interval {
                months: 14,
                days: 3,
                micros: 4_000_000,
            }),
            Value::Decimal(DecimalValue::parse("-12.75").unwrap()),
            Value::List(vec![Value::Str("a".into()), Value::Int(300)]),
            Value::Map(BTreeMap::from([(
                "k".to_string(),
                Value::List(vec![Value::Float(0.5)]),
            )])),
        ];
        for value in values {
            let json = serde_json::to_string(&value).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&json).unwrap(),
                value,
                "round trip failed for {json}"
            );
        }
    }
}
