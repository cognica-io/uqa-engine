//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The `in_range` support functions of the built-in btree operator families, which decide where a `RANGE` frame with an offset starts and ends: whether `val` lies on the `less` side of `base` moved by `offset`, down when `sub` and up otherwise. Each follows its `PostgreSQL` 18 function for the ordering type, including the treatment of NaN, infinities and overflow.

use std::cmp::Ordering;

use uqa_core::memory::ProductionControl;
use uqa_core::{DecimalValue, TemporalValue, Value};

use crate::error::{Result, SQLError};

const MICROS_PER_DAY: i64 = 86_400_000_000;

/// `in_range(val, base, offset, sub, less)` for a non-null `val` and `base` of one ordering type and an `offset` of the type `transformFrameOffset` selected for it.
pub fn in_range(val: &Value, base: &Value, offset: &Value, sub: bool, less: bool) -> Result<bool> {
    match (val, base, offset) {
        (Value::Int(val), Value::Int(base), Value::Int(offset)) => {
            integer_in_range(*val, *base, *offset, sub, less)
        }
        (Value::Float(val), Value::Float(base), Value::Float(offset)) => {
            float_in_range(*val, *base, *offset, sub, less)
        }
        (Value::Decimal(val), Value::Decimal(base), Value::Decimal(offset)) => {
            numeric_in_range(val, base, offset, sub, less)
        }
        (Value::Temporal(val), Value::Temporal(base), Value::Temporal(offset)) => {
            temporal_in_range(val, base, offset, sub, less)
        }
        _ => Err(SQLError::Internal(format!(
            "RANGE frame offset {offset:?} does not apply to ordering values {val:?} and {base:?}"
        ))),
    }
}

fn invalid_offset() -> SQLError {
    SQLError::Routine {
        sqlstate: "22013".into(),
        message: "invalid preceding or following size in window function".into(),
    }
}

fn out_of_range(kind: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22008".into(),
        message: format!("{kind} out of range"),
    }
}

fn bound(ordering: Ordering, less: bool) -> bool {
    if less {
        ordering.is_le()
    } else {
        ordering.is_ge()
    }
}

/// `in_range_int2_int2` through `in_range_int8_int8`. The sum is exact here, which gives the answer those functions give when their narrower sum overflows.
fn integer_in_range(val: i64, base: i64, offset: i64, sub: bool, less: bool) -> Result<bool> {
    if offset < 0 {
        return Err(invalid_offset());
    }
    let offset = i128::from(offset);
    let target = if sub {
        i128::from(base) - offset
    } else {
        i128::from(base) + offset
    };
    Ok(bound(i128::from(val).cmp(&target), less))
}

/// `in_range_float8_float8` and `in_range_float4_float8`.
fn float_in_range(val: f64, base: f64, offset: f64, sub: bool, less: bool) -> Result<bool> {
    if offset.is_nan() || offset < 0.0 {
        return Err(invalid_offset());
    }
    // NaN sorts after every other value; the offset cannot change that.
    if val.is_nan() {
        return Ok(if base.is_nan() { true } else { !less });
    }
    if base.is_nan() {
        return Ok(less);
    }
    // An infinite offset moving an infinite base toward the other infinity would give NaN; every value is taken to lie within such a frame.
    if offset.is_infinite() && base.is_infinite() && (if sub { base > 0.0 } else { base < 0.0 }) {
        return Ok(true);
    }
    let target = if sub { base - offset } else { base + offset };
    Ok(if less { val <= target } else { val >= target })
}

/// `in_range_numeric_numeric`.
fn numeric_in_range(
    val: &DecimalValue,
    base: &DecimalValue,
    offset: &DecimalValue,
    sub: bool,
    less: bool,
) -> Result<bool> {
    if offset.is_nan() || offset.is_negative_infinity() || offset.is_negative() {
        return Err(invalid_offset());
    }
    if val.is_nan() {
        return Ok(if base.is_nan() { true } else { !less });
    }
    if base.is_nan() {
        return Ok(less);
    }
    if offset.is_positive_infinity() {
        if if sub {
            base.is_positive_infinity()
        } else {
            base.is_negative_infinity()
        } {
            return Ok(true);
        }
        return Ok(if sub {
            // base - offset is -Infinity.
            !less || val.is_negative_infinity()
        } else {
            // base + offset is +Infinity.
            less || val.is_positive_infinity()
        });
    }
    if val.is_infinite() {
        return Ok(if val.is_positive_infinity() {
            base.is_positive_infinity() || !less
        } else {
            base.is_negative_infinity() || less
        });
    }
    if base.is_infinite() {
        return Ok(if base.is_negative_infinity() {
            !less
        } else {
            less
        });
    }
    let target = if sub {
        base.checked_sub(offset)
    } else {
        base.checked_add(offset)
    }
    .ok_or_else(|| out_of_range("value"))?;
    Ok(bound(val.cmp(&target), less))
}

/// `in_range_date_interval`, `in_range_timestamp_interval`, `in_range_timestamptz_interval`, `in_range_time_interval`, `in_range_timetz_interval` and `in_range_interval_interval`.
fn temporal_in_range(
    val: &TemporalValue,
    base: &TemporalValue,
    offset: &TemporalValue,
    sub: bool,
    less: bool,
) -> Result<bool> {
    use TemporalValue as T;
    let T::Interval {
        months,
        days,
        micros,
    } = *offset
    else {
        return Err(SQLError::Internal(format!(
            "RANGE frame offset {offset:?} over {base:?} is not an interval"
        )));
    };
    match (val, base) {
        (T::Time { micros: val }, T::Time { micros: base }) => {
            // Like time +/- interval, only the time field of the offset counts, and the sum does not wrap around midnight.
            if micros < 0 {
                return Err(invalid_offset());
            }
            let target = if sub {
                base - micros
            } else {
                match base.checked_add(micros) {
                    Some(target) => target,
                    None => return Ok(less),
                }
            };
            Ok(bound(val.cmp(&target), less))
        }
        (
            T::TimeTz { .. },
            T::TimeTz {
                micros: base,
                offset_minutes,
            },
        ) => {
            if micros < 0 {
                return Err(invalid_offset());
            }
            let target = if sub {
                base - micros
            } else {
                match base.checked_add(micros) {
                    Some(target) => target,
                    None => return Ok(less),
                }
            };
            let target = Value::Temporal(T::TimeTz {
                micros: target,
                offset_minutes: *offset_minutes,
            });
            let ordering = crate::expr::compare_typed_values_with_control(
                &Value::Temporal(val.clone()),
                &target,
                &ProductionControl::uncontrolled(),
            )?;
            Ok(bound(ordering, less))
        }
        (
            T::Interval {
                months: val_months,
                days: val_days,
                micros: val_micros,
            },
            T::Interval {
                months: base_months,
                days: base_days,
                micros: base_micros,
            },
        ) => {
            if interval_span(months, days, micros) < 0 {
                return Err(invalid_offset());
            }
            let (months, days, micros) = signed_interval(months, days, micros, sub)?;
            let target_months = base_months
                .checked_add(months)
                .ok_or_else(|| out_of_range("interval"))?;
            let target_days = base_days
                .checked_add(days)
                .ok_or_else(|| out_of_range("interval"))?;
            let target_micros = base_micros
                .checked_add(micros)
                .ok_or_else(|| out_of_range("interval"))?;
            let ordering = interval_span(*val_months, *val_days, *val_micros).cmp(&interval_span(
                target_months,
                target_days,
                target_micros,
            ));
            Ok(bound(ordering, less))
        }
        _ => {
            if interval_span(months, days, micros) < 0 {
                return Err(invalid_offset());
            }
            let val = timestamp_micros(val)?;
            let base = timestamp_micros(base)?;
            let (months, days, micros) = signed_interval(months, days, micros, sub)?;
            let target = super::time::timestamp_plus_interval(base, months, days, micros)?;
            Ok(bound(val.cmp(&target), less))
        }
    }
}

/// The interval as `interval_cmp_value` orders it: a month is 30 days and a day is 24 hours.
fn interval_span(months: i32, days: i32, micros: i64) -> i128 {
    (i128::from(months) * 30 + i128::from(days)) * i128::from(MICROS_PER_DAY) + i128::from(micros)
}

/// The offset to add: the interval itself, or its negation as `interval_um` computes it.
fn signed_interval(months: i32, days: i32, micros: i64, sub: bool) -> Result<(i32, i32, i64)> {
    if !sub {
        return Ok((months, days, micros));
    }
    match (
        months.checked_neg(),
        days.checked_neg(),
        micros.checked_neg(),
    ) {
        (Some(months), Some(days), Some(micros)) => Ok((months, days, micros)),
        _ => Err(out_of_range("interval")),
    }
}

/// A date as the timestamp at its midnight, as `date2timestamp` converts it, or a timestamp's own microseconds.
fn timestamp_micros(value: &TemporalValue) -> Result<i64> {
    match value {
        TemporalValue::Date { days } => Ok(i64::from(*days) * MICROS_PER_DAY),
        TemporalValue::Timestamp { micros } | TemporalValue::TimestampTz { micros } => Ok(*micros),
        other => Err(SQLError::Internal(format!(
            "RANGE frame ordering value {other:?} is not a date or timestamp"
        ))),
    }
}

#[cfg(test)]
mod tests;
