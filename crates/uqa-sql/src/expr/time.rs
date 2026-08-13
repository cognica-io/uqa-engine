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
use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
use std::time::{SystemTime, UNIX_EPOCH};
use uqa_core::{DecimalValue, TemporalValue, Value};

use crate::ast::BinaryOp;
use crate::error::{Result, SQLError};

use super::{division_by_zero, float_to_i64_rounded, out_of_range, to_f64};

const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_MINUTE: i64 = 60 * MICROS_PER_SECOND;
const MICROS_PER_HOUR: i64 = 3_600 * MICROS_PER_SECOND;
const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;
const NANOS_PER_MICROSECOND: i64 = 1_000;
const NANOS_PER_MILLISECOND: i64 = 1_000_000;
const UUID_V7_SUBMILLISECOND_BITS: u32 = 12;
const UUID_V7_MAX_UNIX_MILLISECONDS: i64 = 0x0000_ffff_ffff_ffff;

#[cfg(any(target_os = "macos", target_os = "windows"))]
const UUID_V7_CLOCK_PRECISION_BITS: u32 = 10;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const UUID_V7_CLOCK_PRECISION_BITS: u32 = 12;

const UUID_V7_MINIMUM_STEP_NANOS: i64 =
    NANOS_PER_MILLISECOND / (1_i64 << UUID_V7_CLOCK_PRECISION_BITS) + 1;
static UUID_V7_PREVIOUS_NANOS: AtomicI64 = AtomicI64::new(0);

fn epoch_date() -> NaiveDate {
    DateTime::<Utc>::UNIX_EPOCH.date_naive()
}

fn naive_from_micros(micros: i64) -> Result<NaiveDateTime> {
    chrono::DateTime::from_timestamp_micros(micros)
        .map(|dt| dt.naive_utc())
        .ok_or_else(|| out_of_range("timestamp"))
}

fn micros_from_naive(naive: NaiveDateTime) -> i64 {
    naive.and_utc().timestamp_micros()
}

/// Shift a date by whole months, clamping the day-of-month to the end
/// of the target month exactly like `PostgreSQL` (`Jan 31 + 1 mon` ->
/// `Feb 29` in a leap year).
fn shift_months(date: NaiveDate, months: i32) -> Result<NaiveDate> {
    let total = i64::from(date.year())
        .checked_mul(12)
        .and_then(|value| value.checked_add(i64::from(date.month0())))
        .and_then(|value| value.checked_add(i64::from(months)))
        .ok_or_else(|| out_of_range("date"))?;
    let year = i32::try_from(total.div_euclid(12)).map_err(|_| out_of_range("date"))?;
    let month = u32::try_from(total.rem_euclid(12)).map_err(|_| out_of_range("date"))? + 1;
    let day = date.day();
    let last = days_in_month(year, month);
    NaiveDate::from_ymd_opt(year, month, day.min(last)).ok_or_else(|| out_of_range("date"))
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
fn timestamp_plus_interval(ts_micros: i64, months: i32, days: i32, micros: i64) -> Result<i64> {
    let naive = naive_from_micros(ts_micros)?;
    let date = shift_months(naive.date(), months)?;
    let date = date
        .checked_add_signed(chrono::Duration::days(i64::from(days)))
        .ok_or_else(|| out_of_range("timestamp"))?;
    let shifted = NaiveDateTime::new(date, naive.time());
    micros_from_naive(shifted)
        .checked_add(micros)
        .ok_or_else(|| out_of_range("timestamp"))
}

/// Binary arithmetic when either operand is temporal. Handles the full
/// `PostgreSQL` matrix used by the engine: date/int, date/date,
/// temporal/interval, timestamp/timestamp, and interval scaling.
pub(super) fn temporal_arith(a: &Value, b: &Value, op: BinaryOp) -> Result<Value> {
    use TemporalValue as T;
    match (a, b) {
        (Value::Temporal(x), Value::Temporal(y)) => match (x, y, op) {
            (T::Date { days: d1 }, T::Date { days: d2 }, BinaryOp::Subtract) => {
                Ok(Value::Int(i64::from(*d1) - i64::from(*d2)))
            }
            (
                T::Interval {
                    months: m1,
                    days: d1,
                    micros: u1,
                },
                T::Interval {
                    months: m2,
                    days: d2,
                    micros: u2,
                },
                BinaryOp::Add | BinaryOp::Subtract,
            ) => {
                let sign = if matches!(op, BinaryOp::Add) { 1 } else { -1 };
                Ok(Value::Temporal(T::Interval {
                    months: m1
                        .checked_add(sign * m2)
                        .ok_or_else(|| out_of_range("interval"))?,
                    days: d1
                        .checked_add(sign * d2)
                        .ok_or_else(|| out_of_range("interval"))?,
                    micros: u1
                        .checked_add(i64::from(sign) * u2)
                        .ok_or_else(|| out_of_range("interval"))?,
                }))
            }
            (
                _,
                T::Interval {
                    months,
                    days,
                    micros,
                },
                BinaryOp::Add,
            ) => add_interval_to_temporal(x, *months, *days, *micros),
            (
                _,
                T::Interval {
                    months,
                    days,
                    micros,
                },
                BinaryOp::Subtract,
            ) => add_interval_to_temporal(x, -months, -days, -micros),
            (
                T::Interval {
                    months,
                    days,
                    micros,
                },
                _,
                BinaryOp::Add,
            ) => add_interval_to_temporal(y, *months, *days, *micros),
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
                    .ok_or_else(|| out_of_range("interval"))?;
                // PostgreSQL justifies full 24h chunks into days but
                // never synthesizes months from a timestamp difference.
                Ok(Value::Temporal(T::Interval {
                    months: 0,
                    days: i32::try_from(diff / MICROS_PER_DAY)
                        .map_err(|_| out_of_range("interval"))?,
                    micros: diff % MICROS_PER_DAY,
                }))
            }
            _ => Err(SQLError::TypeMismatch(format!(
                "unsupported temporal arithmetic: {a:?} {op:?} {b:?}"
            ))),
        },
        // date +/- integer days.
        (Value::Temporal(T::Date { days }), Value::Int(n)) => match op {
            BinaryOp::Add => i64::from(*days)
                .checked_add(*n)
                .ok_or_else(|| out_of_range("date"))
                .and_then(date_value),
            BinaryOp::Subtract => i64::from(*days)
                .checked_sub(*n)
                .ok_or_else(|| out_of_range("date"))
                .and_then(date_value),
            _ => Err(SQLError::TypeMismatch(format!(
                "unsupported temporal arithmetic: {a:?} {op:?} {b:?}"
            ))),
        },
        (Value::Int(n), Value::Temporal(T::Date { days })) if matches!(op, BinaryOp::Add) => n
            .checked_add(i64::from(*days))
            .ok_or_else(|| out_of_range("date"))
            .and_then(date_value),
        // interval * number / number * interval.
        (
            Value::Temporal(T::Interval {
                months,
                days,
                micros,
            }),
            other,
        ) if matches!(op, BinaryOp::Multiply | BinaryOp::Divide) => {
            let factor = to_f64(other)?;
            let factor = if matches!(op, BinaryOp::Divide) {
                if factor == 0.0 {
                    return Err(division_by_zero());
                }
                1.0 / factor
            } else {
                factor
            };
            Ok(Value::Temporal(scale_interval(
                *months, *days, *micros, factor,
            )?))
        }
        (
            other,
            Value::Temporal(T::Interval {
                months,
                days,
                micros,
            }),
        ) if matches!(op, BinaryOp::Multiply) => {
            let factor = to_f64(other)?;
            Ok(Value::Temporal(scale_interval(
                *months, *days, *micros, factor,
            )?))
        }
        _ => Err(SQLError::TypeMismatch(format!(
            "unsupported temporal arithmetic: {a:?} {op:?} {b:?}"
        ))),
    }
}

fn date_value(days: i64) -> Result<Value> {
    Ok(Value::Temporal(TemporalValue::Date {
        days: i32::try_from(days).map_err(|_| out_of_range("date"))?,
    }))
}

/// Multiply an interval by a factor, cascading fractional months into
/// days and fractional days into microseconds (`PostgreSQL`
/// `interval_mul` semantics).
fn scale_interval(months: i32, days: i32, micros: i64, factor: f64) -> Result<TemporalValue> {
    if !factor.is_finite() {
        return Err(out_of_range("interval"));
    }
    let month_total = f64::from(months) * factor;
    let month_whole = month_total.trunc();
    let day_total = f64::from(days) * factor + (month_total - month_whole) * 30.0;
    let day_whole = day_total.trunc();
    let micro_total = micros as f64 * factor + (day_total - day_whole) * MICROS_PER_DAY as f64;
    if !month_whole.is_finite()
        || month_whole < f64::from(i32::MIN)
        || month_whole >= 2_147_483_648.0
        || !day_whole.is_finite()
        || day_whole < f64::from(i32::MIN)
        || day_whole >= 2_147_483_648.0
    {
        return Err(out_of_range("interval"));
    }
    Ok(TemporalValue::Interval {
        months: month_whole as i32,
        days: day_whole as i32,
        micros: float_to_i64_rounded(micro_total, "interval")?,
    })
}

fn add_interval_to_temporal(
    base: &TemporalValue,
    months: i32,
    days: i32,
    micros: i64,
) -> Result<Value> {
    use TemporalValue as T;
    match base {
        // date +/- interval promotes to timestamp in PostgreSQL.
        T::Date { days: base_days } => {
            let ts = i64::from(*base_days) * MICROS_PER_DAY;
            Ok(Value::Temporal(T::Timestamp {
                micros: timestamp_plus_interval(ts, months, days, micros)?,
            }))
        }
        T::Timestamp { micros: ts } => Ok(Value::Temporal(T::Timestamp {
            micros: timestamp_plus_interval(*ts, months, days, micros)?,
        })),
        T::TimestampTz { micros: ts } => Ok(Value::Temporal(T::TimestampTz {
            micros: timestamp_plus_interval(*ts, months, days, micros)?,
        })),
        // time +/- interval wraps within the day; months/days vanish.
        T::Time { micros: t } => Ok(Value::Temporal(T::Time {
            micros: wrap_time(*t, micros),
        })),
        T::TimeTz {
            micros: t,
            offset_minutes,
        } => Ok(Value::Temporal(T::TimeTz {
            micros: wrap_time(*t, micros),
            offset_minutes: *offset_minutes,
        })),
        T::Interval { .. } => Err(SQLError::TypeMismatch(
            "cannot add interval to interval through this path".into(),
        )),
    }
}

fn wrap_time(left: i64, right: i64) -> i64 {
    (i128::from(left) + i128::from(right)).rem_euclid(i128::from(MICROS_PER_DAY)) as i64
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
    match v {
        Value::Temporal(t) => Ok(t.clone()),
        Value::Str(s) => TemporalValue::parse_timestamp(s)
            .or_else(|| TemporalValue::parse_date(s))
            .or_else(|| TemporalValue::parse_time(s))
            .or_else(|| TemporalValue::parse_interval(s))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot parse timestamp {s:?}"))),
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
            .map_err(|_| out_of_range("interval"))?,
        days: i32::try_from(sign * days).map_err(|_| out_of_range("interval"))?,
        micros: sign * time,
    }))
}

/// Numeric result with a fixed decimal scale (`extract(epoch ...)`
/// renders `60.000000`).
fn decimal_scaled(value: f64, scale: u32) -> Value {
    let text = format!("{value:.*}", scale as usize);
    DecimalValue::parse(&text).map_or(Value::Float(value), Value::Decimal)
}

/// `EXTRACT(field FROM x)` / `date_part(field, x)`. `as_numeric`
/// selects EXTRACT's numeric result type (epoch/second render with a
/// fixed scale); `date_part` keeps float8 semantics.
pub(super) fn extract_from_value(field: &str, value: &Value, as_numeric: bool) -> Result<Value> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    let temporal = coerce_temporal(value)?;
    let int_result = |n: i64| Ok(Value::Int(n));
    let seconds_result = |micros: i64| {
        let secs = micros as f64 / 1e6;
        if as_numeric {
            Ok(decimal_scaled(secs, 6))
        } else {
            Ok(Value::Float(secs))
        }
    };
    if let TemporalValue::Interval {
        months,
        days,
        micros,
    } = &temporal
    {
        return match field {
            "year" | "years" => int_result(i64::from(months / 12)),
            "month" | "months" | "mon" | "mons" => int_result(i64::from(months % 12)),
            "day" | "days" => int_result(i64::from(*days)),
            "hour" | "hours" => int_result(micros / MICROS_PER_HOUR),
            "minute" | "minutes" => int_result((micros % MICROS_PER_HOUR) / MICROS_PER_MINUTE),
            "second" | "seconds" => {
                let sub = micros % MICROS_PER_MINUTE;
                if as_numeric {
                    Ok(decimal_scaled(sub as f64 / 1e6, 6))
                } else {
                    Ok(Value::Float(sub as f64 / 1e6))
                }
            }
            "millisecond" | "milliseconds" => {
                let sub = micros % MICROS_PER_MINUTE;
                if as_numeric {
                    Ok(decimal_scaled(sub as f64 / 1e3, 3))
                } else {
                    Ok(Value::Float(sub as f64 / 1e3))
                }
            }
            "microsecond" | "microseconds" => int_result(micros % MICROS_PER_MINUTE),
            "epoch" => {
                let total = (i64::from(*months) * 30 + i64::from(*days)) * MICROS_PER_DAY + micros;
                if as_numeric {
                    Ok(decimal_scaled(total as f64 / 1e6, 6))
                } else {
                    Ok(Value::Float(total as f64 / 1e6))
                }
            }
            "quarter" => {
                let quarter = if *months < 0 {
                    (months % 12) / 3 - 1
                } else {
                    (months % 12) / 3 + 1
                };
                int_result(i64::from(quarter))
            }
            "week" | "weeks" => int_result(i64::from(days / 7)),
            other => Err(SQLError::Unsupported(format!("EXTRACT field `{other}`"))),
        };
    }
    if let TemporalValue::Time { micros } | TemporalValue::TimeTz { micros, .. } = &temporal {
        return match field {
            "hour" | "hours" => int_result(micros / MICROS_PER_HOUR),
            "minute" | "minutes" => int_result((micros % MICROS_PER_HOUR) / MICROS_PER_MINUTE),
            "second" | "seconds" => seconds_result(micros % MICROS_PER_MINUTE),
            "millisecond" | "milliseconds" => {
                let sub = micros % MICROS_PER_MINUTE;
                if as_numeric {
                    Ok(decimal_scaled(sub as f64 / 1e3, 3))
                } else {
                    Ok(Value::Float(sub as f64 / 1e3))
                }
            }
            "microsecond" | "microseconds" => int_result(micros % MICROS_PER_MINUTE),
            "epoch" => seconds_result(*micros),
            other => Err(SQLError::Unsupported(format!("EXTRACT field `{other}`"))),
        };
    }
    let dt = temporal_naive(&temporal)?;
    match field {
        "year" => int_result(i64::from(dt.year())),
        "month" => int_result(i64::from(dt.month())),
        "day" => int_result(i64::from(dt.day())),
        "hour" => int_result(i64::from(dt.hour())),
        "minute" => int_result(i64::from(dt.minute())),
        "second" => {
            let micros = i64::from(dt.second()) * MICROS_PER_SECOND
                + i64::from(dt.and_utc().timestamp_subsec_micros());
            if as_numeric {
                Ok(decimal_scaled(micros as f64 / 1e6, 6))
            } else {
                Ok(Value::Float(micros as f64 / 1e6))
            }
        }
        "millisecond" | "milliseconds" => {
            let micros = i64::from(dt.second()) * MICROS_PER_SECOND
                + i64::from(dt.and_utc().timestamp_subsec_micros());
            if as_numeric {
                Ok(decimal_scaled(micros as f64 / 1e3, 3))
            } else {
                Ok(Value::Float(micros as f64 / 1e3))
            }
        }
        "microsecond" | "microseconds" => int_result(
            i64::from(dt.second()) * MICROS_PER_SECOND
                + i64::from(dt.and_utc().timestamp_subsec_micros()),
        ),
        "dow" => int_result(i64::from(dt.weekday().num_days_from_sunday())),
        "isodow" => int_result(i64::from(dt.weekday().number_from_monday())),
        "doy" => int_result(i64::from(dt.ordinal())),
        "epoch" => {
            let micros = micros_from_naive(dt);
            if as_numeric {
                Ok(decimal_scaled(micros as f64 / 1e6, 6))
            } else {
                let secs = micros as f64 / 1e6;
                if secs.fract() == 0.0 {
                    int_result(secs as i64)
                } else {
                    Ok(Value::Float(secs))
                }
            }
        }
        "quarter" => int_result(i64::from(dt.month() - 1) / 3 + 1),
        "week" => int_result(i64::from(dt.iso_week().week())),
        "isoyear" => int_result(i64::from(dt.iso_week().year())),
        "century" => int_result(i64::from((dt.year() + 99).div_euclid(100))),
        "decade" => int_result(i64::from(dt.year().div_euclid(10))),
        "millennium" => int_result(i64::from((dt.year() + 999).div_euclid(1000))),
        other => Err(SQLError::Unsupported(format!("EXTRACT field `{other}`"))),
    }
}

pub(super) fn date_trunc_value(unit: &str, value: &Value) -> Result<Value> {
    let temporal = coerce_temporal(value)?;
    let tz = matches!(temporal, TemporalValue::TimestampTz { .. });
    let dt = temporal_naive(&temporal)?;
    let date = dt.date();
    let truncated = match unit {
        "millennium" => with_date(NaiveDate::from_ymd_opt(
            (date.year() - 1) / 1000 * 1000 + 1,
            1,
            1,
        )),
        "century" => with_date(NaiveDate::from_ymd_opt(
            (date.year() - 1) / 100 * 100 + 1,
            1,
            1,
        )),
        "decade" => with_date(NaiveDate::from_ymd_opt(date.year() / 10 * 10, 1, 1)),
        "year" => with_date(NaiveDate::from_ymd_opt(date.year(), 1, 1)),
        "quarter" => {
            let month = (date.month() - 1) / 3 * 3 + 1;
            with_date(NaiveDate::from_ymd_opt(date.year(), month, 1))
        }
        "month" => with_date(NaiveDate::from_ymd_opt(date.year(), date.month(), 1)),
        "week" => {
            let offset = date.weekday().num_days_from_monday() as i64;
            with_date(date.checked_sub_signed(chrono::Duration::days(offset)))
        }
        "day" => with_date(Some(date)),
        "hour" => date.and_hms_opt(dt.hour(), 0, 0),
        "minute" => date.and_hms_opt(dt.hour(), dt.minute(), 0),
        "second" => date.and_hms_opt(dt.hour(), dt.minute(), dt.second()),
        "milliseconds" => {
            let millis = i64::from(dt.and_utc().timestamp_subsec_millis());
            date.and_hms_opt(dt.hour(), dt.minute(), dt.second())
                .map(|naive| naive + chrono::Duration::milliseconds(millis))
        }
        "microseconds" => Some(dt),
        other => {
            return Err(SQLError::Unsupported(format!("date_trunc unit `{other}`")));
        }
    }
    .ok_or_else(|| SQLError::TypeMismatch(format!("date_trunc: bad {unit}")))?;
    let micros = micros_from_naive(truncated);
    Ok(Value::Temporal(if tz {
        TemporalValue::TimestampTz { micros }
    } else {
        TemporalValue::Timestamp { micros }
    }))
}

fn with_date(date: Option<NaiveDate>) -> Option<NaiveDateTime> {
    date.and_then(|d| d.and_hms_opt(0, 0, 0))
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
        .ok_or_else(|| out_of_range("timestamp"))?;
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

pub(super) fn format_pg_number(n: f64, fmt: &str) -> String {
    // Minimal `to_char(numeric, '999...')` support: count `9` digits
    // and zero-pad the integral part. Falls back to plain Display.
    let digits = fmt.chars().filter(|c| *c == '9' || *c == '0').count();
    if digits == 0 {
        return n.to_string();
    }
    if fmt.contains('.') {
        let frac_digits = fmt.split('.').nth(1).map(str::len).unwrap_or(0);
        format!("{n:.frac_digits$}")
    } else {
        let truncated = n.trunc();
        format!("{truncated:0digits$.0}")
    }
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

pub(super) fn generate_random_uuid() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| SQLError::Internal(format!("failed to obtain random bytes: {error}")))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format_uuid(bytes))
}

pub(super) fn generate_uuid_v7(shift: Option<&TemporalValue>) -> Result<String> {
    let now_nanos = real_time_nanos_ascending()?;
    let now_micros = now_nanos.div_euclid(NANOS_PER_MICROSECOND);
    let sub_microsecond_nanos = now_nanos.rem_euclid(NANOS_PER_MICROSECOND);
    let timestamp_micros = match shift {
        None => now_micros,
        Some(TemporalValue::Interval {
            months,
            days,
            micros,
        }) => timestamp_plus_interval(now_micros, *months, *days, *micros)?,
        Some(other) => {
            return Err(SQLError::TypeMismatch(format!(
                "uuidv7: expected interval, got {other:?}"
            )));
        }
    };
    let unix_millis = timestamp_micros.div_euclid(1_000);
    if !(0..=UUID_V7_MAX_UNIX_MILLISECONDS).contains(&unix_millis) {
        return Err(out_of_range("uuidv7 timestamp"));
    }
    let sub_millisecond_nanos = timestamp_micros
        .rem_euclid(1_000)
        .checked_mul(NANOS_PER_MICROSECOND)
        .and_then(|nanos| nanos.checked_add(sub_microsecond_nanos))
        .ok_or_else(|| out_of_range("uuidv7 timestamp"))?;
    let sub_millisecond_nanos =
        u32::try_from(sub_millisecond_nanos).map_err(|_| out_of_range("uuidv7 timestamp"))?;
    generate_uuid_v7_at(unix_millis as u64, sub_millisecond_nanos)
}

fn real_time_nanos_ascending() -> Result<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| out_of_range("uuidv7 timestamp"))?;
    let actual = i64::try_from(elapsed.as_nanos()).map_err(|_| out_of_range("uuidv7 timestamp"))?;
    loop {
        let previous = UUID_V7_PREVIOUS_NANOS.load(AtomicOrdering::Relaxed);
        let minimum = previous
            .checked_add(UUID_V7_MINIMUM_STEP_NANOS)
            .ok_or_else(|| out_of_range("uuidv7 timestamp"))?;
        let candidate = if minimum >= actual { minimum } else { actual };
        if UUID_V7_PREVIOUS_NANOS
            .compare_exchange_weak(
                previous,
                candidate,
                AtomicOrdering::Relaxed,
                AtomicOrdering::Relaxed,
            )
            .is_ok()
        {
            return Ok(candidate);
        }
    }
}

fn generate_uuid_v7_at(unix_millis: u64, sub_millisecond_nanos: u32) -> Result<String> {
    if unix_millis > UUID_V7_MAX_UNIX_MILLISECONDS as u64
        || sub_millisecond_nanos >= NANOS_PER_MILLISECOND as u32
    {
        return Err(out_of_range("uuidv7 timestamp"));
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes[8..])
        .map_err(|error| SQLError::Internal(format!("failed to obtain random bytes: {error}")))?;
    let timestamp = unix_millis.to_be_bytes();
    bytes[..6].copy_from_slice(&timestamp[2..]);
    let increased_clock_precision = (u64::from(sub_millisecond_nanos)
        * (1_u64 << UUID_V7_SUBMILLISECOND_BITS))
        / NANOS_PER_MILLISECOND as u64;
    bytes[6] = (increased_clock_precision >> 8) as u8;
    bytes[7] = increased_clock_precision as u8;

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        bytes[7] ^= bytes[8] >> 6;
    }

    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format_uuid(bytes))
}

fn format_uuid(bytes: [u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

pub(super) fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
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
