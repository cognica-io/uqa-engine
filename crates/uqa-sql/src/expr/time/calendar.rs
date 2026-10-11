//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Timestamp interval arithmetic preserves each calendar step before applying elapsed time.

use super::{
    datetime_out_of_range, infinite, micros_from_naive, naive_from_micros, shift_months,
    IntervalFields, Result, MICROS_PER_DAY, MICROS_PER_SECOND,
};
use crate::expr::EngineHook;
use uqa_core::{memory::ProductionControl, TemporalTimeZone, TemporalValue};

pub(super) fn timestamp_plus_interval(ts: i64, span: IntervalFields) -> Result<i64> {
    advance(ts, span, None)
}

pub(in crate::expr) fn timestamp_tz_plus_interval(
    ts: i64,
    span: IntervalFields,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<i64> {
    control.check()?;
    let timestamp = TemporalValue::TimestampTz { micros: ts };
    // Infinite operands and elapsed-time-only intervals do not consult a zone.
    if !timestamp.is_finite() || !span.value().is_finite() || span.months == 0 && span.days == 0 {
        return advance(ts, span, None);
    }
    let zone = engine
        .map(EngineHook::temporal_time_zone)
        .transpose()?
        .flatten()
        .unwrap_or_else(|| TemporalTimeZone::fixed(0).expect("UTC offset"));
    control.check()?;
    advance(ts, span, Some(zone))
}

fn advance(mut ts: i64, span: IntervalFields, zone: Option<TemporalTimeZone>) -> Result<i64> {
    let timestamp = TemporalValue::Timestamp { micros: ts };
    if let Some(infinity) =
        infinite::add(timestamp.infinity(), span.value().infinity(), "timestamp")?
    {
        return Ok(infinite::timestamp_micros(infinity));
    }
    if span.months != 0 {
        let local = naive_from_micros(local_time(ts, zone)?)?;
        let date = shift_months(local.date(), span.months)?;
        ts = from_local(micros_from_naive(date.and_time(local.time())), zone)?;
    }
    if span.days != 0 {
        // A month step can normalize a missing civil time into the next hour or day.
        // PostgreSQL derives local time again before applying the day field.
        let local = local_time(ts, zone)?;
        let julian = local.div_euclid(MICROS_PER_DAY) + 2_440_588 + i64::from(span.days);
        let minimum = if zone.is_some() { -1 } else { 0 };
        if !(minimum..=i64::from(i32::MAX)).contains(&julian) {
            return Err(datetime_out_of_range("timestamp"));
        }
        let local = i128::from(local) + i128::from(span.days) * i128::from(MICROS_PER_DAY);
        ts = from_local(
            i64::try_from(local).map_err(|_| datetime_out_of_range("timestamp"))?,
            zone,
        )?;
    }
    ts.checked_add(span.micros)
        .filter(|micros| TemporalValue::timestamp_micros_in_range(*micros))
        .ok_or_else(|| datetime_out_of_range("timestamp"))
}

fn local_time(ts: i64, zone: Option<TemporalTimeZone>) -> Result<i64> {
    let offset = match zone {
        Some(zone) => zone
            .offset_at(ts.div_euclid(MICROS_PER_SECOND))
            .ok_or_else(|| datetime_out_of_range("timestamp"))?,
        None => 0,
    };
    ts.checked_add(i64::from(offset) * MICROS_PER_SECOND)
        .ok_or_else(|| datetime_out_of_range("timestamp"))
}

fn from_local(local: i64, zone: Option<TemporalTimeZone>) -> Result<i64> {
    let offset = match zone {
        Some(zone) => zone
            .offset_for_local(local.div_euclid(MICROS_PER_SECOND))
            .ok_or_else(|| datetime_out_of_range("timestamp"))?,
        None => 0,
    };
    local
        .checked_sub(i64::from(offset) * MICROS_PER_SECOND)
        .filter(|micros| TemporalValue::timestamp_micros_in_range(*micros))
        .ok_or_else(|| datetime_out_of_range("timestamp"))
}

#[cfg(test)]
mod tests;
