//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal arithmetic, formatting, UUID, and hex helpers for scalar
//! functions. Mirrors `PostgreSQL` 18 semantics: `date + int` stays a
//! date, `date - date` counts days, intervals use the
//! months/days/micros model, and `age()` produces the symbolic
//! year/month decomposition.

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, Timelike, Utc};
use uqa_core::{memory::ProductionControl, TemporalValue, Value};

use crate::ast::BinaryOp;
use crate::error::{Result, SQLError};

use super::conversion::to_f64_with_control;
use super::{datetime_out_of_range, float_to_i64_rounded, out_of_range};

mod date_trunc;
mod extract;
mod interval;
mod number_format;
mod units;

pub(super) use date_trunc::zone::{truncate_explicit_zone, truncate_session_zone};
pub(super) use extract::{extract_from_value, extract_from_value_with_offset};
pub use interval::IntervalFields;

const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_MINUTE: i64 = 60 * MICROS_PER_SECOND;
const MICROS_PER_HOUR: i64 = 3_600 * MICROS_PER_SECOND;
const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;
fn epoch_date() -> NaiveDate {
    DateTime::<Utc>::UNIX_EPOCH.date_naive()
}

fn naive_from_micros(micros: i64) -> Result<NaiveDateTime> {
    chrono::DateTime::from_timestamp_micros(micros)
        .map(|dt| dt.naive_utc())
        .ok_or_else(|| datetime_out_of_range("timestamp"))
}

fn micros_from_naive(naive: NaiveDateTime) -> i64 {
    naive.and_utc().timestamp_micros()
}

/// Shift the date of a timestamp by whole months, clamping the day-of-month to the end
/// of the target month exactly like `PostgreSQL` (`Jan 31 + 1 mon` ->
/// `Feb 29` in a leap year). `timestamp_pl_interval` reports any overflow as the timestamp's.
fn shift_months(date: NaiveDate, months: i32) -> Result<NaiveDate> {
    let total = i64::from(date.year())
        .checked_mul(12)
        .and_then(|value| value.checked_add(i64::from(date.month0())))
        .and_then(|value| value.checked_add(i64::from(months)))
        .ok_or_else(|| datetime_out_of_range("timestamp"))?;
    let year =
        i32::try_from(total.div_euclid(12)).map_err(|_| datetime_out_of_range("timestamp"))?;
    let month =
        u32::try_from(total.rem_euclid(12)).map_err(|_| datetime_out_of_range("timestamp"))? + 1;
    let day = date.day();
    let last = days_in_month(year, month);
    NaiveDate::from_ymd_opt(year, month, day.min(last))
        .ok_or_else(|| datetime_out_of_range("timestamp"))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if NaiveDate::from_ymd_opt(year, 2, 29).is_some() {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// `timestamp + interval` (calendar-aware): months first with day
/// clamping, then days, then the sub-day microseconds.
pub(super) fn timestamp_plus_interval(
    ts_micros: i64,
    months: i32,
    days: i32,
    micros: i64,
) -> Result<i64> {
    let naive = naive_from_micros(ts_micros)?;
    let date = shift_months(naive.date(), months)?;
    let date = date
        .checked_add_signed(chrono::Duration::days(i64::from(days)))
        .ok_or_else(|| datetime_out_of_range("timestamp"))?;
    let shifted = NaiveDateTime::new(date, naive.time());
    micros_from_naive(shifted)
        .checked_add(micros)
        .ok_or_else(|| datetime_out_of_range("timestamp"))
}

/// Binary arithmetic when either operand is temporal. Handles the full
/// `PostgreSQL` matrix used by the engine: date/int, date/date,
/// temporal/interval, timestamp/timestamp, and interval scaling.
pub(super) fn temporal_arith_with_control(
    a: &Value,
    b: &Value,
    op: BinaryOp,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    control.check()?;
    let to_f64 = |value| to_f64_with_control(value, control);
    use TemporalValue as T;
    match (a, b) {
        (Value::Temporal(x), Value::Temporal(y)) => temporal_pair_arith(a, b, x, y, op),
        // date +/- integer days.
        (Value::Temporal(T::Date { days }), Value::Int(n)) => match op {
            BinaryOp::Add => i64::from(*days)
                .checked_add(*n)
                .ok_or_else(|| datetime_out_of_range("date"))
                .and_then(date_value),
            BinaryOp::Subtract => i64::from(*days)
                .checked_sub(*n)
                .ok_or_else(|| datetime_out_of_range("date"))
                .and_then(date_value),
            _ => Err(SQLError::TypeMismatch(format!(
                "unsupported temporal arithmetic: {a:?} {op:?} {b:?}"
            ))),
        },
        (Value::Int(n), Value::Temporal(T::Date { days })) if matches!(op, BinaryOp::Add) => n
            .checked_add(i64::from(*days))
            .ok_or_else(|| datetime_out_of_range("date"))
            .and_then(date_value),
        // interval * float8, interval / float8 and float8 * interval.
        (Value::Temporal(interval @ T::Interval { .. }), other)
            if matches!(op, BinaryOp::Multiply | BinaryOp::Divide) =>
        {
            let (interval, factor) = (interval_fields(interval)?, to_f64(other)?);
            Ok(Value::Temporal(
                if matches!(op, BinaryOp::Divide) {
                    interval.divide(factor)?
                } else {
                    interval.multiply(factor)?
                }
                .value(),
            ))
        }
        (other, Value::Temporal(interval @ T::Interval { .. }))
            if matches!(op, BinaryOp::Multiply) =>
        {
            let factor = to_f64(other)?;
            Ok(Value::Temporal(
                interval_fields(interval)?.multiply(factor)?.value(),
            ))
        }
        _ => Err(SQLError::TypeMismatch(format!(
            "unsupported temporal arithmetic: {a:?} {op:?} {b:?}"
        ))),
    }
}

/// Arithmetic on two temporal values: `date - date` counts days, intervals add and subtract, a date or timestamp takes an interval, `date + time` is `datetime_pl`, two instants subtract to an interval, and two times subtract to an interval.
fn temporal_pair_arith(
    a: &Value,
    b: &Value,
    x: &TemporalValue,
    y: &TemporalValue,
    op: BinaryOp,
) -> Result<Value> {
    use TemporalValue as T;
    match (x, y, op) {
        (T::Date { days: d1 }, T::Date { days: d2 }, BinaryOp::Subtract) => {
            Ok(Value::Int(i64::from(*d1) - i64::from(*d2)))
        }
        (T::Interval { .. }, T::Interval { .. }, BinaryOp::Add | BinaryOp::Subtract) => {
            let (left, right) = (interval_fields(x)?, interval_fields(y)?);
            Ok(Value::Temporal(
                if matches!(op, BinaryOp::Add) {
                    left.plus(right)?
                } else {
                    left.minus(right)?
                }
                .value(),
            ))
        }
        // `datetime_pl`, `timedate_pl`, `datetimetz_pl` and `timetzdate_pl`: the time of day on the date; a time with time zone names the instant through its offset.
        (T::Date { days }, T::Time { micros }, BinaryOp::Add)
        | (T::Time { micros }, T::Date { days }, BinaryOp::Add) => {
            Ok(Value::Temporal(T::Timestamp {
                micros: date_time_micros(*days, *micros, 0)?,
            }))
        }
        (
            T::Date { days },
            T::TimeTz {
                micros,
                offset_minutes,
            },
            BinaryOp::Add,
        )
        | (
            T::TimeTz {
                micros,
                offset_minutes,
            },
            T::Date { days },
            BinaryOp::Add,
        ) => Ok(Value::Temporal(T::TimestampTz {
            micros: date_time_micros(
                *days,
                *micros,
                i64::from(*offset_minutes) * MICROS_PER_MINUTE,
            )?,
        })),
        (_, T::Interval { .. }, BinaryOp::Add) => {
            add_interval_to_temporal(x, interval_fields(y)?, false)
        }
        (_, T::Interval { .. }, BinaryOp::Subtract) => {
            add_interval_to_temporal(x, interval_fields(y)?, true)
        }
        (T::Interval { .. }, _, BinaryOp::Add) => {
            add_interval_to_temporal(y, interval_fields(x)?, false)
        }
        (T::Time { micros: t1 }, T::Time { micros: t2 }, BinaryOp::Subtract) => {
            Ok(Value::Temporal(T::Interval {
                months: 0,
                days: 0,
                micros: t1 - t2,
            }))
        }
        (_, _, BinaryOp::Subtract) => {
            let lhs = temporal_timestamp_micros(x)?;
            let rhs = temporal_timestamp_micros(y)?;
            let diff = lhs
                .checked_sub(rhs)
                .ok_or_else(|| datetime_out_of_range("interval"))?;
            // PostgreSQL justifies full 24h chunks into days but
            // never synthesizes months from a timestamp difference.
            Ok(Value::Temporal(T::Interval {
                months: 0,
                days: i32::try_from(diff / MICROS_PER_DAY)
                    .map_err(|_| datetime_out_of_range("interval"))?,
                micros: diff % MICROS_PER_DAY,
            }))
        }
        _ => Err(SQLError::TypeMismatch(format!(
            "unsupported temporal arithmetic: {a:?} {op:?} {b:?}"
        ))),
    }
}

/// The instant at `time` of day on the date `days` after the epoch, `offset` microseconds east of UTC, or `timestamp out of range` when it leaves the carrier.
fn date_time_micros(days: i32, time: i64, offset: i64) -> Result<i64> {
    i64::from(days)
        .checked_mul(MICROS_PER_DAY)
        .and_then(|date| date.checked_add(time))
        .and_then(|local| local.checked_sub(offset))
        .ok_or_else(|| datetime_out_of_range("timestamp"))
}

fn date_value(days: i64) -> Result<Value> {
    Ok(Value::Temporal(TemporalValue::Date {
        days: i32::try_from(days).map_err(|_| datetime_out_of_range("date"))?,
    }))
}

fn interval_fields(value: &TemporalValue) -> Result<IntervalFields> {
    IntervalFields::of(value)
        .ok_or_else(|| SQLError::Internal(format!("{value:?} is not an interval")))
}

/// `timestamp_pl_interval`, `timestamp_mi_interval` and their date, time and time with time zone counterparts. Subtraction from a date or timestamp adds the negated interval, while `time_mi_interval` and `timetz_mi_interval` subtract the time field alone.
fn add_interval_to_temporal(
    base: &TemporalValue,
    span: IntervalFields,
    subtract: bool,
) -> Result<Value> {
    use TemporalValue as T;
    let calendar_span = || if subtract { span.negate() } else { Ok(span) };
    match base {
        // date +/- interval promotes to timestamp in PostgreSQL.
        T::Date { days: base_days } => {
            let ts = i64::from(*base_days) * MICROS_PER_DAY;
            let span = calendar_span()?;
            Ok(Value::Temporal(T::Timestamp {
                micros: timestamp_plus_interval(ts, span.months, span.days, span.micros)?,
            }))
        }
        T::Timestamp { micros: ts } => {
            let span = calendar_span()?;
            Ok(Value::Temporal(T::Timestamp {
                micros: timestamp_plus_interval(*ts, span.months, span.days, span.micros)?,
            }))
        }
        T::TimestampTz { micros: ts } => {
            let span = calendar_span()?;
            Ok(Value::Temporal(T::TimestampTz {
                micros: timestamp_plus_interval(*ts, span.months, span.days, span.micros)?,
            }))
        }
        // time +/- interval wraps within the day; months/days vanish.
        T::Time { micros: t } => Ok(Value::Temporal(T::Time {
            micros: wrap_time(*t, span.micros, subtract),
        })),
        T::TimeTz {
            micros: t,
            offset_minutes,
        } => Ok(Value::Temporal(T::TimeTz {
            micros: wrap_time(*t, span.micros, subtract),
            offset_minutes: *offset_minutes,
        })),
        T::Interval { .. } => Err(SQLError::TypeMismatch(
            "cannot add interval to interval through this path".into(),
        )),
    }
}

/// The time of day `time_pl_interval` and `time_mi_interval` compute: the 64-bit sum wraps as `PostgreSQL`'s `-fwrapv` build wraps it before it is reduced to one day.
fn wrap_time(time: i64, span: i64, subtract: bool) -> i64 {
    let result = if subtract {
        time.wrapping_sub(span)
    } else {
        time.wrapping_add(span)
    };
    result.rem_euclid(MICROS_PER_DAY)
}

/// Absolute timestamp microseconds for datetime-like temporal values.
fn temporal_timestamp_micros(t: &TemporalValue) -> Result<i64> {
    use TemporalValue as T;
    match t {
        T::Date { days } => Ok(i64::from(*days) * MICROS_PER_DAY),
        T::Timestamp { micros } | T::TimestampTz { micros } => Ok(*micros),
        other => Err(SQLError::TypeMismatch(format!(
            "expected date or timestamp, got {other:?}"
        ))),
    }
}

/// Coerce a scalar into a datetime-like temporal value: temporal
/// values pass through, strings parse as timestamp / date / time.
pub(super) fn coerce_temporal(v: &Value) -> Result<TemporalValue> {
    coerce_temporal_with_control(v, &ProductionControl::uncontrolled())
}

pub(super) fn coerce_temporal_with_control(
    v: &Value,
    control: &ProductionControl<'_>,
) -> Result<TemporalValue> {
    control.check()?;
    match v {
        Value::Temporal(t) => Ok(t.clone()),
        Value::Str(s) => {
            let now_micros = crate::expr::transaction_timestamp_or_clock();
            for parse in [
                TemporalValue::timestamp_input_in_order_with_control,
                TemporalValue::date_input_in_order_with_control,
                TemporalValue::time_input_in_order_with_control,
            ] {
                if let Ok(value) =
                    parse(s, now_micros, crate::expr::temporal_date_order(), control)?
                {
                    return Ok(value);
                }
            }
            if let Some(value) = TemporalValue::parse_interval_with_control(s, control)? {
                return Ok(value);
            }
            Err(SQLError::TypeMismatch(format!(
                "cannot parse timestamp {s:?}"
            )))
        }
        other => Err(SQLError::TypeMismatch(format!(
            "expected timestamp, got {other:?}"
        ))),
    }
}

fn temporal_naive(t: &TemporalValue) -> Result<NaiveDateTime> {
    naive_from_micros(temporal_timestamp_micros(t)?)
}

/// `age(a, b)`: symbolic year/month/day decomposition of `a - b`,
/// borrowing across fields exactly like `PostgreSQL`'s
/// `timestamp_age`.
pub(super) fn age_between(a: &TemporalValue, b: &TemporalValue) -> Result<Value> {
    let am = temporal_timestamp_micros(a)?;
    let bm = temporal_timestamp_micros(b)?;
    let sign: i64 = if am >= bm { 1 } else { -1 };
    let (hi, lo) = if am >= bm { (am, bm) } else { (bm, am) };
    let hi_dt = naive_from_micros(hi)?;
    let lo_dt = naive_from_micros(lo)?;
    let mut years = i64::from(hi_dt.year()) - i64::from(lo_dt.year());
    let mut months = i64::from(hi_dt.month()) - i64::from(lo_dt.month());
    let mut days = i64::from(hi_dt.day()) - i64::from(lo_dt.day());
    let time_of = |dt: NaiveDateTime| -> i64 {
        i64::from(dt.num_seconds_from_midnight()) * MICROS_PER_SECOND
            + i64::from(dt.and_utc().timestamp_subsec_micros())
    };
    let mut time = time_of(hi_dt) - time_of(lo_dt);
    if time < 0 {
        time += MICROS_PER_DAY;
        days -= 1;
    }
    if days < 0 {
        days += i64::from(days_in_month(lo_dt.year(), lo_dt.month()));
        months -= 1;
    }
    if months < 0 {
        months += 12;
        years -= 1;
    }
    Ok(Value::Temporal(TemporalValue::Interval {
        months: i32::try_from(sign * (years * 12 + months))
            .map_err(|_| datetime_out_of_range("interval"))?,
        days: i32::try_from(sign * days).map_err(|_| datetime_out_of_range("interval"))?,
        micros: sign * time,
    }))
}

pub(super) fn date_trunc_value(
    unit: &str,
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    date_trunc::truncate(unit, value, control)
}

pub(super) fn make_timestamp(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: f64,
) -> Result<Value> {
    if !second.is_finite() || !(0.0..60.0).contains(&second) {
        return Err(SQLError::TypeMismatch(
            "make_timestamp: seconds must be finite and between 0 and 60".into(),
        ));
    }
    let date = NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| SQLError::TypeMismatch("make_timestamp: bad date".into()))?;
    let base = date
        .and_hms_opt(hour, minute, 0)
        .ok_or_else(|| SQLError::TypeMismatch("make_timestamp: bad time".into()))?;
    let micros = float_to_i64_rounded(second * MICROS_PER_SECOND as f64, "time")?;
    let naive = base
        .checked_add_signed(chrono::Duration::microseconds(micros))
        .ok_or_else(|| datetime_out_of_range("timestamp"))?;
    Ok(Value::Temporal(TemporalValue::Timestamp {
        micros: micros_from_naive(naive),
    }))
}

pub(super) fn pg_to_chrono_fmt(fmt: &str) -> String {
    // Translate a small subset of PostgreSQL `to_date` template tokens
    // into chrono format specifiers, covering the common `YYYY`, `MM`, `DD`,
    // `HH24`, `MI`, and `SS` patterns.
    fmt.replace("YYYY", "%Y")
        .replace("YY", "%y")
        .replace("Month", "%B")
        .replace("Mon", "%b")
        .replace("MM", "%m")
        .replace("Day", "%A")
        .replace("Dy", "%a")
        .replace("DDD", "%j")
        .replace("DD", "%d")
        .replace("HH24", "%H")
        .replace("HH12", "%I")
        .replace("MI", "%M")
        .replace("SS", "%S")
        .replace("US", "%6f")
        .replace("MS", "%3f")
        .replace("AM", "%p")
        .replace("PM", "%p")
}

pub(super) fn format_pg_number(value: &Value, fmt: &str) -> Result<String> {
    number_format::format_pg_number(value, fmt)
}

/// `to_char(temporal, fmt)` for typed temporal values.
pub(super) fn format_temporal(value: &TemporalValue, fmt: &str) -> Result<String> {
    use TemporalValue as T;
    let naive = match value {
        T::Date { .. } | T::Timestamp { .. } | T::TimestampTz { .. } => temporal_naive(value)?,
        T::Time { micros } | T::TimeTz { micros, .. } => {
            let time = chrono::NaiveTime::from_num_seconds_from_midnight_opt(
                (micros.rem_euclid(MICROS_PER_DAY) / MICROS_PER_SECOND) as u32,
                ((micros.rem_euclid(MICROS_PER_DAY) % MICROS_PER_SECOND) * 1_000) as u32,
            )
            .ok_or_else(|| SQLError::TypeMismatch("to_char: bad time".into()))?;
            NaiveDateTime::new(epoch_date(), time)
        }
        T::Interval { .. } => {
            return Err(SQLError::Unsupported("to_char(interval, text)".into()));
        }
    };
    Ok(naive.format(&pg_to_chrono_fmt(fmt)).to_string())
}

pub(super) fn hex_encode(bytes: &[u8]) -> String {
    super::encoding::hex_encode_with_control(
        bytes,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary hex encoding")
    .into_uncontrolled()
    .expect("ordinary hex owner")
}

/// Parse a timestamp string into a UTC `DateTime`. Accepts:
/// - RFC3339 (`2025-01-31T12:00:00Z`, `2025-01-31T12:00:00+09:00`)
/// - PostgreSQL-ish `YYYY-MM-DD HH:MM:SS[.fff]` (assumed UTC)
/// - Bare date `YYYY-MM-DD` (midnight UTC).
pub(super) fn parse_timestamp(s: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    use chrono::{TimeZone, Utc};
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    let formats: &[&str] = &[
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f%#z",
    ];
    for fmt in formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(Utc.from_utc_datetime(&naive));
        }
    }
    if let Ok(dt) = chrono::DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%#z") {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let naive = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| SQLError::TypeMismatch(format!("bad date {s}")))?;
        return Ok(Utc.from_utc_datetime(&naive));
    }
    Err(SQLError::TypeMismatch(format!(
        "cannot parse timestamp {s:?}"
    )))
}
