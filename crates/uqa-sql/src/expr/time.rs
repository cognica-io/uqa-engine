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

use chrono::{NaiveDate, NaiveDateTime};
use uqa_core::{memory::ProductionControl, TemporalDate, TemporalDateTime, TemporalValue, Value};

use crate::ast::BinaryOp;
use crate::error::{Result, SQLError};

use super::conversion::to_f64_with_control;
use super::{datetime_out_of_range, float_to_i64_rounded, out_of_range};

mod calendar;
mod construct;
pub(super) use construct::{make_date, make_timestamp};
mod date_trunc;
mod extract;
mod format;
mod infinite;
mod interval;
mod number_format;
mod timezone;
mod units;

pub(super) use calendar::timestamp_tz_plus_interval;
pub(super) use date_trunc::zone::{truncate_explicit_zone, truncate_session_zone};
pub(super) use extract::{extract_from_value, extract_from_value_with_offset};
pub use interval::IntervalFields;
pub(super) use timezone::eval_timezone;

const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_MINUTE: i64 = 60 * MICROS_PER_SECOND;
const MICROS_PER_HOUR: i64 = 3_600 * MICROS_PER_SECOND;
const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;

fn timestamp_fields(micros: i64) -> TemporalDateTime {
    TemporalDateTime::from_postgres_micros(micros)
}

fn timestamp_micros(value: TemporalDateTime) -> Result<i64> {
    value
        .postgres_micros()
        .ok_or_else(|| datetime_out_of_range("timestamp"))
}

/// Shift the date of a timestamp by whole months, clamping the day-of-month to the end
/// of the target month exactly like `PostgreSQL` (`Jan 31 + 1 mon` ->
/// `Feb 29` in a leap year). `timestamp_pl_interval` reports any overflow as the timestamp's.
fn shift_months(date: TemporalDate, months: i32) -> Result<TemporalDate> {
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
    TemporalDate::from_calendar(year, month, day.min(last))
        .ok_or_else(|| datetime_out_of_range("timestamp"))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    TemporalDate::days_in_month(year, month)
}

/// `timestamp + interval` (calendar-aware): months first with day
/// clamping, then days, then the sub-day microseconds.
pub(super) fn timestamp_plus_interval(
    ts_micros: i64,
    months: i32,
    days: i32,
    micros: i64,
) -> Result<i64> {
    calendar::timestamp_plus_interval(
        ts_micros,
        IntervalFields {
            months,
            days,
            micros,
        },
    )
}

/// Binary arithmetic when either operand is temporal. Handles the full
/// `PostgreSQL` matrix used by the engine: date/int, date/date,
/// temporal/interval, timestamp/timestamp, and interval scaling.
pub(super) fn temporal_arith_with_control(
    a: &Value,
    b: &Value,
    op: BinaryOp,
    engine: Option<&dyn super::EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    control.check()?;
    let to_f64 = |value| to_f64_with_control(value, control);
    use TemporalValue as T;
    match (a, b) {
        (Value::Temporal(x), Value::Temporal(y)) => {
            temporal_pair_arith(a, b, x, y, op, engine, control)
        }
        (Value::Temporal(date @ T::Date { .. }), Value::Int(_))
            if !date.is_finite() && matches!(op, BinaryOp::Add | BinaryOp::Subtract) =>
        {
            Ok(Value::Temporal(date.clone()))
        }
        (Value::Int(_), Value::Temporal(date @ T::Date { .. }))
            if !date.is_finite() && matches!(op, BinaryOp::Add) =>
        {
            Ok(Value::Temporal(date.clone()))
        }
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
    engine: Option<&dyn super::EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    use TemporalValue as T;
    match (x, y, op) {
        (T::Date { days: d1 }, T::Date { days: d2 }, BinaryOp::Subtract) => {
            if !x.is_finite() || !y.is_finite() {
                return Err(SQLError::Routine {
                    sqlstate: "22008".into(),
                    message: "cannot subtract infinite dates".into(),
                });
            }
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
                postgres_micros: date_time_micros(*days, *micros, 0)?,
            }))
        }
        (
            T::Date { days },
            T::TimeTz {
                micros,
                offset_seconds,
            },
            BinaryOp::Add,
        )
        | (
            T::TimeTz {
                micros,
                offset_seconds,
            },
            T::Date { days },
            BinaryOp::Add,
        ) => Ok(Value::Temporal(T::TimestampTz {
            postgres_micros: date_time_micros(
                *days,
                *micros,
                i64::from(*offset_seconds) * MICROS_PER_SECOND,
            )?,
        })),
        (_, T::Interval { .. }, BinaryOp::Add) => {
            add_interval_to_temporal(x, interval_fields(y)?, false, engine, control)
        }
        (_, T::Interval { .. }, BinaryOp::Subtract) => {
            add_interval_to_temporal(x, interval_fields(y)?, true, engine, control)
        }
        (T::Interval { .. }, _, BinaryOp::Add) => {
            add_interval_to_temporal(y, interval_fields(x)?, false, engine, control)
        }
        (T::Time { micros: t1 }, T::Time { micros: t2 }, BinaryOp::Subtract) => {
            Ok(Value::Temporal(T::Interval {
                months: 0,
                days: 0,
                micros: t1 - t2,
            }))
        }
        (_, _, BinaryOp::Subtract) => subtract_timestamps(x, y),
        _ => Err(SQLError::TypeMismatch(format!(
            "unsupported temporal arithmetic: {a:?} {op:?} {b:?}"
        ))),
    }
}

fn subtract_timestamps(x: &TemporalValue, y: &TemporalValue) -> Result<Value> {
    let lhs = temporal_timestamp_micros(x)?;
    let rhs = temporal_timestamp_micros(y)?;
    if let Some(infinity) = infinite::subtract(x.infinity(), y.infinity())? {
        return Ok(Value::Temporal(infinity.interval()));
    }
    let diff = lhs
        .checked_sub(rhs)
        .ok_or_else(|| datetime_out_of_range("interval"))?;
    // PostgreSQL justifies full 24h chunks into days but
    // never synthesizes months from a timestamp difference.
    Ok(Value::Temporal(TemporalValue::Interval {
        months: 0,
        days: i32::try_from(diff / MICROS_PER_DAY)
            .map_err(|_| datetime_out_of_range("interval"))?,
        micros: diff % MICROS_PER_DAY,
    }))
}

/// The instant at `time` of day on the date `days` after the epoch, `offset` microseconds east of UTC, or `timestamp out of range` when it leaves the carrier.
fn date_time_micros(days: i32, time: i64, offset: i64) -> Result<i64> {
    if let Some(infinity) = (TemporalValue::Date { days }).infinity() {
        return Ok(infinite::timestamp_micros(infinity));
    }
    let micros = (i128::from(days) - i128::from(uqa_core::POSTGRES_EPOCH_DAYS))
        * i128::from(MICROS_PER_DAY)
        + i128::from(time)
        - i128::from(offset);
    i64::try_from(micros)
        .ok()
        .filter(|value| TemporalValue::timestamp_micros_in_range(*value))
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
    engine: Option<&dyn super::EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    use TemporalValue as T;
    if matches!(base, T::Time { .. } | T::TimeTz { .. }) && !span.value().is_finite() {
        return Err(SQLError::Routine {
            sqlstate: "22008".into(),
            message: if subtract {
                "cannot subtract infinite interval from time"
            } else {
                "cannot add infinite interval to time"
            }
            .into(),
        });
    }
    let calendar_span = || if subtract { span.negate() } else { Ok(span) };
    match base {
        // date +/- interval promotes to timestamp in PostgreSQL.
        T::Date { days: base_days } => {
            let ts = date_time_micros(*base_days, 0, 0)?;
            let span = calendar_span()?;
            Ok(Value::Temporal(T::Timestamp {
                postgres_micros: timestamp_plus_interval(ts, span.months, span.days, span.micros)?,
            }))
        }
        T::Timestamp {
            postgres_micros: ts,
        } => {
            let span = calendar_span()?;
            Ok(Value::Temporal(T::Timestamp {
                postgres_micros: timestamp_plus_interval(*ts, span.months, span.days, span.micros)?,
            }))
        }
        T::TimestampTz {
            postgres_micros: ts,
        } => {
            let span = calendar_span()?;
            Ok(Value::Temporal(T::TimestampTz {
                postgres_micros: timestamp_tz_plus_interval(*ts, span, engine, control)?,
            }))
        }
        // time +/- interval wraps within the day; months/days vanish.
        T::Time { micros: t } => Ok(Value::Temporal(T::Time {
            micros: wrap_time(*t, span.micros, subtract),
        })),
        T::TimeTz {
            micros: t,
            offset_seconds,
        } => Ok(Value::Temporal(T::TimeTz {
            micros: wrap_time(*t, span.micros, subtract),
            offset_seconds: *offset_seconds,
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
pub(super) fn temporal_timestamp_micros(t: &TemporalValue) -> Result<i64> {
    use TemporalValue as T;
    match t {
        T::Date { days } => date_time_micros(*days, 0, 0),
        T::Timestamp {
            postgres_micros: micros,
        }
        | T::TimestampTz {
            postgres_micros: micros,
        } => Ok(*micros),
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

fn temporal_fields(t: &TemporalValue) -> Result<TemporalDateTime> {
    Ok(timestamp_fields(temporal_timestamp_micros(t)?))
}

/// `age(a, b)`: symbolic year/month/day decomposition of `a - b`,
/// borrowing across fields exactly like `PostgreSQL`'s
/// `timestamp_age`.
pub(super) fn age_between(a: &TemporalValue, b: &TemporalValue) -> Result<Value> {
    let am = temporal_timestamp_micros(a)?;
    let bm = temporal_timestamp_micros(b)?;
    if let Some(infinity) = infinite::subtract(a.infinity(), b.infinity())? {
        return Ok(Value::Temporal(infinity.interval()));
    }
    let sign: i64 = if am >= bm { 1 } else { -1 };
    let (hi, lo) = if am >= bm { (am, bm) } else { (bm, am) };
    let hi_dt = timestamp_fields(hi);
    let lo_dt = timestamp_fields(lo);
    let mut years = i64::from(hi_dt.year()) - i64::from(lo_dt.year());
    let mut months = i64::from(hi_dt.month()) - i64::from(lo_dt.month());
    let mut days = i64::from(hi_dt.day()) - i64::from(lo_dt.day());
    let mut time = hi_dt.time_micros() - lo_dt.time_micros();
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
    format_temporal_with_control(value, fmt, &ProductionControl::uncontrolled()).map(|value| {
        value
            .into_uncontrolled()
            .expect("ordinary temporal formatting")
    })
}

pub(super) fn format_temporal_with_control(
    value: &TemporalValue,
    fmt: &str,
    control: &ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<String>> {
    control.check()?;
    use TemporalValue as T;
    let fields = match value {
        T::Date { days } => TemporalDate::from_unix_days(*days)
            .at_time(0)
            .expect("midnight"),
        T::Timestamp { .. } | T::TimestampTz { .. } => temporal_fields(value)?,
        T::Time { micros } | T::TimeTz { micros, .. } => TemporalDate::from_unix_days(0)
            .at_time(micros.rem_euclid(MICROS_PER_DAY))
            .expect("normalized time"),
        T::Interval { .. } => return Err(SQLError::Unsupported("to_char(interval, text)".into())),
    };
    Ok(format::format(fields, fmt, control)?)
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
/// - `PostgreSQL`-ish `YYYY-MM-DD HH:MM:SS[.fff]` (assumed UTC)
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
