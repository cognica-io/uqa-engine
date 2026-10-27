//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL calendar constructors preserve era conversion, field diagnostics and microsecond rounding order.

use super::{
    float_to_i64_rounded, Result, SQLError, TemporalDate, TemporalValue, Value, MICROS_PER_DAY,
    MICROS_PER_SECOND,
};

fn error(message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: "22008".into(),
        message,
    }
}

fn calendar(year: i32, month: i32, day: i32, timestamp: bool) -> Result<TemporalDate> {
    let astronomical = if year < 0 { year + 1 } else { year };
    let displayed = if timestamp || year == i32::MIN {
        year
    } else {
        astronomical
    };
    let field_error = || {
        error(format!(
            "date field value out of range: {displayed}-{month:02}-{day:02}"
        ))
    };
    if year == 0 || year == i32::MIN || !(1..=12).contains(&month) {
        return Err(field_error());
    }
    let month_number = u32::try_from(month).expect("validated month");
    if day < 1
        || u32::try_from(day).unwrap_or(u32::MAX)
            > TemporalDate::days_in_month(astronomical, month_number)
    {
        return Err(field_error());
    }
    let range_error = || {
        error(format!(
            "date out of range: {displayed}-{month:02}-{day:02}"
        ))
    };
    if (astronomical, month) < (-4713, 11) || (astronomical, month) >= (5_874_898, 6) {
        return Err(range_error());
    }
    TemporalDate::from_calendar(
        astronomical,
        month_number,
        u32::try_from(day).expect("validated day"),
    )
    .ok_or_else(range_error)
}

pub(in crate::expr) fn make_date(year: i32, month: i32, day: i32) -> Result<Value> {
    let date = calendar(year, month, day, false)?;
    if !TemporalValue::date_days_in_range(date.unix_days()) {
        let astronomical = if year < 0 { year + 1 } else { year };
        return Err(error(format!(
            "date out of range: {astronomical}-{month:02}-{day:02}"
        )));
    }
    Ok(Value::Temporal(TemporalValue::Date {
        days: date.unix_days(),
    }))
}

pub(in crate::expr) fn make_timestamp(
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    minute: i32,
    second: f64,
) -> Result<Value> {
    let date = calendar(year, month, day, true)?;
    let time_error = || {
        error(format!(
            "time field value out of range: {hour}:{minute:02}:{}",
            diagnostic_seconds(second)
        ))
    };
    let rounded = (second * MICROS_PER_SECOND as f64).round_ties_even();
    if !(0..=24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0.0..=60_000_000.0).contains(&rounded)
    {
        return Err(time_error());
    }
    let time = (i64::from(hour) * 3600 + i64::from(minute) * 60) * MICROS_PER_SECOND
        + float_to_i64_rounded(rounded, "time")?;
    if time > MICROS_PER_DAY {
        return Err(time_error());
    }
    let postgres_micros = date
        .at_time(0)
        .expect("midnight")
        .postgres_micros()
        .and_then(|value| value.checked_add(time))
        .filter(|value| TemporalValue::timestamp_micros_in_range(*value))
        .ok_or_else(|| {
            error(format!(
                "timestamp out of range: {year}-{month:02}-{day:02} {hour}:{minute:02}:{}",
                diagnostic_seconds(second)
            ))
        })?;
    Ok(Value::Temporal(TemporalValue::Timestamp {
        postgres_micros,
    }))
}

/// `PostgreSQL`'s diagnostic %02g uses six significant digits and a minimum width of two.
fn diagnostic_seconds(value: f64) -> String {
    if !value.is_finite() {
        return uqa_core::format_float_pg(value);
    }
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific.split_once('e').expect("scientific formatting");
    let exponent: i32 = exponent.parse().expect("finite floating exponent");
    let mut text = if (-4..6).contains(&exponent) {
        let precision = usize::try_from((5 - exponent).max(0)).expect("bounded float precision");
        let text = format!("{value:.precision$}");
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            text
        }
    } else {
        format!(
            "{}e{exponent:+03}",
            mantissa.trim_end_matches('0').trim_end_matches('.')
        )
    };
    if text.len() < 2 {
        text.insert(0, '0');
    }
    text
}

#[cfg(test)]
mod tests;
