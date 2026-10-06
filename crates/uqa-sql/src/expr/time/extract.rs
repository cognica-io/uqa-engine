//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Finite temporal extraction keeps exact numeric coefficients separate from floating-point evaluation.

use chrono::{Datelike, NaiveDate, Timelike};
use uqa_core::{
    memory::{Produced, ProductionControl},
    TemporalValue, Value,
};

use crate::error::Result;

use super::{
    coerce_temporal_with_control, datetime_out_of_range, epoch_date, naive_from_micros,
    units::{Unit, UnitName},
    MICROS_PER_DAY, MICROS_PER_HOUR, MICROS_PER_MINUTE, MICROS_PER_SECOND,
};

mod result;
use result::Part;

/// Extract in UTC when no session time-zone capability is supplied.
pub(in crate::expr) fn extract_from_value(
    field: &str,
    value: &Value,
    as_numeric: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    extract_from_value_with_offset(field, value, as_numeric, 0, control)
}

/// `offset_seconds` is east of UTC and affects only local fields of `timestamptz`; epoch retains the original instant.
pub(in crate::expr) fn extract_from_value_with_offset(
    field: &str,
    value: &Value,
    as_numeric: bool,
    offset_seconds: i32,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    if matches!(value, Value::Null) {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let temporal = coerce_temporal_with_control(value, control)?;
    let type_name = extraction_type_name(&temporal, as_numeric);
    let name = UnitName::new(field);
    let unit = name
        .extraction()
        .ok_or_else(|| name.unrecognized(type_name))?;
    let part = match temporal {
        TemporalValue::Interval {
            months,
            days,
            micros,
        } => interval_part(unit, months, days, micros),
        TemporalValue::Time { micros } => time_part(unit, micros, None),
        TemporalValue::TimeTz {
            micros,
            offset_minutes,
        } => time_part(unit, micros, Some(i64::from(offset_minutes) * 60)),
        TemporalValue::Date { days } if as_numeric => date_part(unit, days)?,
        TemporalValue::Date { days } => {
            // `date_part(text, date)` is PostgreSQL's SQL wrapper around a midnight timestamp.
            let micros = i64::from(days)
                .checked_mul(MICROS_PER_DAY)
                .ok_or_else(|| datetime_out_of_range("timestamp"))?;
            timestamp_part(unit, micros, None)?
        }
        TemporalValue::Timestamp { micros } => timestamp_part(unit, micros, None)?,
        TemporalValue::TimestampTz { micros } => {
            timestamp_part(unit, micros, Some(offset_seconds))?
        }
    };
    let part = part.ok_or_else(|| {
        if unit == Unit::Reserved
            && matches!(
                temporal,
                TemporalValue::Interval { .. }
                    | TemporalValue::Time { .. }
                    | TemporalValue::TimeTz { .. }
            )
        {
            name.unrecognized(type_name)
        } else {
            name.unsupported(type_name, None)
        }
    })?;
    part.finish(as_numeric, control)
}

fn extraction_type_name(value: &TemporalValue, as_numeric: bool) -> &'static str {
    match value {
        TemporalValue::Date { .. } if as_numeric => "date",
        TemporalValue::Date { .. } | TemporalValue::Timestamp { .. } => {
            "timestamp without time zone"
        }
        TemporalValue::TimestampTz { .. } => "timestamp with time zone",
        TemporalValue::Time { .. } => "time without time zone",
        TemporalValue::TimeTz { .. } => "time with time zone",
        TemporalValue::Interval { .. } => "interval",
    }
}

fn interval_part(unit: Unit, months: i32, days: i32, micros: i64) -> Option<Part> {
    let years = i64::from(months / 12);
    let integer = match unit {
        Unit::Millennium => years / 1_000,
        Unit::Century => years / 100,
        Unit::Decade => years / 10,
        Unit::Year => years,
        Unit::Quarter => {
            if months < 0 {
                // PostgreSQL negates the signed int32 month field before taking its remainder.
                -(i64::from(months.wrapping_neg() % 12) / 3 + 1)
            } else {
                i64::from(months % 12) / 3 + 1
            }
        }
        Unit::Month => i64::from(months % 12),
        Unit::Week => i64::from(days / 7),
        Unit::Day => i64::from(days),
        Unit::Epoch => {
            let seconds =
                (1_461 * years + 120 * i64::from(months % 12) + 4 * i64::from(days)) * 21_600;
            let coefficient =
                i128::from(seconds) * i128::from(MICROS_PER_SECOND) + i128::from(micros);
            // Match interval_part_common's sequence of float operations, including cancellation between fields.
            let mut float = micros as f64 / 1_000_000.0;
            float += 31_557_600.0 * years as f64;
            float += 2_592_000.0 * f64::from(months % 12);
            float += 86_400.0 * f64::from(days);
            return Some(Part::Scaled {
                coefficient,
                scale: 6,
                float,
            });
        }
        _ => return clock_part(unit, micros),
    };
    Some(Part::Integer(integer))
}

fn time_part(unit: Unit, micros: i64, offset_seconds: Option<i64>) -> Option<Part> {
    if unit == Unit::Epoch {
        let offset = offset_seconds.unwrap_or(0);
        return Some(Part::Scaled {
            coefficient: i128::from(micros) - i128::from(offset) * i128::from(MICROS_PER_SECOND),
            scale: 6,
            float: micros as f64 / 1_000_000.0 - offset as f64,
        });
    }
    if let Some(offset) = offset_seconds {
        if let Some(part) = timezone_part(unit, offset) {
            return Some(part);
        }
    }
    clock_part(unit, micros)
}

fn timezone_part(unit: Unit, offset: i64) -> Option<Part> {
    Some(Part::Integer(match unit {
        Unit::Timezone => offset,
        Unit::TimezoneHour => offset / 3_600,
        Unit::TimezoneMinute => (offset / 60) % 60,
        _ => return None,
    }))
}

fn clock_part(unit: Unit, micros: i64) -> Option<Part> {
    let sub = micros % MICROS_PER_MINUTE;
    Some(match unit {
        Unit::Hour => Part::Integer(micros / MICROS_PER_HOUR),
        Unit::Minute => Part::Integer((micros % MICROS_PER_HOUR) / MICROS_PER_MINUTE),
        Unit::Microseconds => Part::Integer(sub),
        Unit::Milliseconds => Part::Scaled {
            coefficient: i128::from(sub),
            scale: 3,
            float: (sub / MICROS_PER_SECOND) as f64 * 1_000.0
                + (sub % MICROS_PER_SECOND) as f64 / 1_000.0,
        },
        Unit::Second => Part::Scaled {
            coefficient: i128::from(sub),
            scale: 6,
            float: (sub / MICROS_PER_SECOND) as f64
                + (sub % MICROS_PER_SECOND) as f64 / 1_000_000.0,
        },
        _ => return None,
    })
}

fn date_part(unit: Unit, days: i32) -> Result<Option<Part>> {
    if unit == Unit::Epoch {
        return Ok(Some(Part::Integer(i64::from(days) * 86_400)));
    }
    let date = epoch_date()
        .checked_add_signed(chrono::Duration::days(i64::from(days)))
        .ok_or_else(|| datetime_out_of_range("date"))?;
    Ok(calendar_part(unit, date).map(Part::Integer))
}

fn timestamp_part(unit: Unit, micros: i64, offset_seconds: Option<i32>) -> Result<Option<Part>> {
    if unit == Unit::Epoch {
        return Ok(Some(Part::Scaled {
            coefficient: i128::from(micros),
            scale: 6,
            float: micros as f64 / 1_000_000.0,
        }));
    }
    let local = micros
        .checked_add(i64::from(offset_seconds.unwrap_or(0)) * MICROS_PER_SECOND)
        .ok_or_else(|| datetime_out_of_range("timestamp"))?;
    let datetime = naive_from_micros(local)?;
    if let Some(offset) = offset_seconds {
        if let Some(part) = timezone_part(unit, i64::from(offset)) {
            return Ok(Some(part));
        }
    }
    let time = i64::from(datetime.num_seconds_from_midnight()) * MICROS_PER_SECOND
        + i64::from(datetime.and_utc().timestamp_subsec_micros());
    if unit == Unit::Julian {
        return Ok(Some(Part::Julian {
            day: julian_day(datetime.date()),
            micros: time,
        }));
    }
    Ok(clock_part(unit, time).or_else(|| calendar_part(unit, datetime.date()).map(Part::Integer)))
}

fn julian_day(date: NaiveDate) -> i64 {
    date.signed_duration_since(epoch_date()).num_days() + 2_440_588
}

fn calendar_part(unit: Unit, date: NaiveDate) -> Option<i64> {
    let year = i64::from(date.year());
    Some(match unit {
        Unit::Millennium => {
            if year > 0 {
                (year + 999) / 1_000
            } else {
                -((1_000 - year) / 1_000)
            }
        }
        Unit::Century => {
            if year > 0 {
                (year + 99) / 100
            } else {
                -((100 - year) / 100)
            }
        }
        Unit::Decade => year.div_euclid(10),
        Unit::Year => {
            if year > 0 {
                year
            } else {
                year - 1
            }
        }
        Unit::Quarter => i64::from(date.month0() / 3 + 1),
        Unit::Month => i64::from(date.month()),
        Unit::Week => i64::from(date.iso_week().week()),
        Unit::Day => i64::from(date.day()),
        Unit::Dow => i64::from(date.weekday().num_days_from_sunday()),
        Unit::IsoDow => i64::from(date.weekday().number_from_monday()),
        Unit::Doy => i64::from(date.ordinal()),
        Unit::IsoYear => {
            let year = i64::from(date.iso_week().year());
            if year > 0 {
                year
            } else {
                year - 1
            }
        }
        Unit::Julian => julian_day(date),
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
