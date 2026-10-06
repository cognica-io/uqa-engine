//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Finite `timestamp_trunc` and `interval_trunc` with `DecodeUnits` names and diagnostics.

use super::{
    coerce_temporal_with_control, datetime_out_of_range, micros_from_naive, temporal_naive,
    units::{Unit, UnitName},
    Datelike, IntervalFields, NaiveDate, NaiveDateTime, ProductionControl, Result, TemporalValue,
    Timelike, Value, MICROS_PER_HOUR, MICROS_PER_MINUTE, MICROS_PER_SECOND,
};

pub(super) mod zone;

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
    let name = UnitName::new(raw);
    let unit = name.units().ok_or_else(|| name.unrecognized(type_name))?;
    if matches!(
        unit,
        Unit::Timezone | Unit::TimezoneHour | Unit::TimezoneMinute
    ) {
        return Err(name.unsupported(type_name, None));
    }
    if unit == Unit::Week && type_name == "interval" {
        return Err(name.unsupported(type_name, Some("Months usually have fractional weeks.")));
    }
    Ok(unit)
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
        _ => unreachable!("timestamp units were validated before truncation"),
    };
    result.ok_or_else(|| datetime_out_of_range("timestamp"))
}

#[cfg(test)]
mod tests;
