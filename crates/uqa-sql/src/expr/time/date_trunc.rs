//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Finite `timestamp_trunc` and `interval_trunc` with `DecodeUnits` names and diagnostics.

use super::{
    coerce_temporal_with_control, datetime_out_of_range, micros_from_naive, temporal_naive,
    Datelike, IntervalFields, NaiveDate, NaiveDateTime, ProductionControl, Result, SQLError,
    TemporalValue, Timelike, Value, MICROS_PER_HOUR, MICROS_PER_MINUTE, MICROS_PER_SECOND,
};

pub(super) mod zone;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Millennium,
    Century,
    Decade,
    Year,
    Quarter,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Milliseconds,
    Microseconds,
}

pub(super) fn truncate(
    units: &str,
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    control.check()?;
    let temporal = coerce_temporal_with_control(value, control)?;
    let type_name = match temporal {
        TemporalValue::Interval { .. } => "interval",
        TemporalValue::TimestampTz { .. } => "timestamp with time zone",
        _ => "timestamp without time zone",
    };
    let unit = decode_unit(units, type_name)?;
    if let Some(interval) = IntervalFields::of(&temporal) {
        return Ok(Value::Temporal(truncate_interval(unit, interval).value()));
    }
    let result = truncate_timestamp(unit, temporal_naive(&temporal)?)?;
    let micros = micros_from_naive(result);
    if !TemporalValue::timestamp_micros_in_range(micros) {
        return Err(datetime_out_of_range("timestamp"));
    }
    Ok(Value::Temporal(
        if matches!(temporal, TemporalValue::TimestampTz { .. }) {
            TemporalValue::TimestampTz { micros }
        } else {
            TemporalValue::Timestamp { micros }
        },
    ))
}

fn decode_unit(raw: &str, type_name: &str) -> Result<Unit> {
    // `downcase_truncate_identifier` clips on a UTF-8 boundary; `DecodeUnits` compares at most ten bytes without trimming whitespace.
    let mut length = raw.len().min(63);
    while !raw.is_char_boundary(length) {
        length -= 1;
    }
    let mut normalized = [0_u8; 63];
    for (output, input) in normalized.iter_mut().zip(&raw.as_bytes()[..length]) {
        *output = input.to_ascii_lowercase();
    }
    let name = std::str::from_utf8(&normalized[..length])
        .expect("ASCII folding and character-boundary truncation preserve UTF-8");
    let token = &normalized[..length.min(10)];
    let unit = match token {
        b"mil" | b"mils" | b"millennia" | b"millennium" => Unit::Millennium,
        b"c" | b"cent" | b"century" | b"centuries" => Unit::Century,
        b"dec" | b"decs" | b"decade" | b"decades" => Unit::Decade,
        b"y" | b"yr" | b"yrs" | b"year" | b"years" => Unit::Year,
        b"qtr" | b"quarter" => Unit::Quarter,
        b"mon" | b"mons" | b"month" | b"months" => Unit::Month,
        b"w" | b"week" | b"weeks" => Unit::Week,
        b"d" | b"day" | b"days" => Unit::Day,
        b"h" | b"hr" | b"hrs" | b"hour" | b"hours" => Unit::Hour,
        b"m" | b"min" | b"mins" | b"minute" | b"minutes" => Unit::Minute,
        b"s" | b"sec" | b"secs" | b"second" | b"seconds" => Unit::Second,
        b"ms" | b"msec" | b"msecs" | b"msecond" | b"mseconds" | b"millisecon" => Unit::Milliseconds,
        b"us" | b"usec" | b"usecs" | b"usecond" | b"useconds" | b"microsecon" => Unit::Microseconds,
        b"timezone" | b"timezone_h" | b"timezone_m" => {
            return Err(unsupported_unit(name, type_name, None));
        }
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "22023".into(),
                message: format!("unit \"{name}\" not recognized for type {type_name}"),
            });
        }
    };
    if unit == Unit::Week && type_name == "interval" {
        return Err(unsupported_unit(
            name,
            type_name,
            Some("Months usually have fractional weeks."),
        ));
    }
    Ok(unit)
}

fn unsupported_unit(name: &str, type_name: &str, detail: Option<&str>) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "0A000".into(),
        message: format!("unit \"{name}\" not supported for type {type_name}"),
        detail: detail.map(str::to_string),
        hint: None,
    }
}

fn truncate_interval(unit: Unit, mut interval: IntervalFields) -> IntervalFields {
    let month_multiple = match unit {
        Unit::Millennium => 12_000,
        Unit::Century => 1_200,
        Unit::Decade => 120,
        Unit::Year => 12,
        Unit::Quarter => 3,
        Unit::Month => 1,
        _ => 0,
    };
    if month_multiple != 0 {
        // Division truncates toward zero, including negative calendar and time fields. Neither hours nor days carry into a larger interval field.
        interval.months = interval.months / month_multiple * month_multiple;
        interval.days = 0;
        interval.micros = 0;
    } else if unit == Unit::Day {
        interval.micros = 0;
    } else {
        let multiple = match unit {
            Unit::Hour => MICROS_PER_HOUR,
            Unit::Minute => MICROS_PER_MINUTE,
            Unit::Second => MICROS_PER_SECOND,
            Unit::Milliseconds => 1_000,
            Unit::Microseconds => 1,
            _ => unreachable!("interval units were validated before truncation"),
        };
        interval.micros = interval.micros / multiple * multiple;
    }
    interval
}

fn truncate_timestamp(unit: Unit, value: NaiveDateTime) -> Result<NaiveDateTime> {
    let date = value.date();
    let calendar_date = |year, month, day| {
        NaiveDate::from_ymd_opt(year, month, day).and_then(|date| date.and_hms_opt(0, 0, 0))
    };
    let result = match unit {
        Unit::Millennium => calendar_date((date.year() - 1).div_euclid(1_000) * 1_000 + 1, 1, 1),
        Unit::Century => calendar_date((date.year() - 1).div_euclid(100) * 100 + 1, 1, 1),
        Unit::Decade => calendar_date(date.year().div_euclid(10) * 10, 1, 1),
        Unit::Year => calendar_date(date.year(), 1, 1),
        Unit::Quarter => calendar_date(date.year(), date.month0() / 3 * 3 + 1, 1),
        Unit::Month => calendar_date(date.year(), date.month(), 1),
        Unit::Week => date
            .checked_sub_signed(chrono::Duration::days(i64::from(
                date.weekday().num_days_from_monday(),
            )))
            .and_then(|date| date.and_hms_opt(0, 0, 0)),
        Unit::Day => date.and_hms_opt(0, 0, 0),
        Unit::Hour => date.and_hms_opt(value.hour(), 0, 0),
        Unit::Minute => date.and_hms_opt(value.hour(), value.minute(), 0),
        Unit::Second => date.and_hms_opt(value.hour(), value.minute(), value.second()),
        Unit::Milliseconds => date.and_hms_micro_opt(
            value.hour(),
            value.minute(),
            value.second(),
            value.and_utc().timestamp_subsec_micros() / 1_000 * 1_000,
        ),
        Unit::Microseconds => Some(value),
    };
    result.ok_or_else(|| datetime_out_of_range("timestamp"))
}

#[cfg(test)]
mod tests;
