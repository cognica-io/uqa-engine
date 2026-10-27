//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal C carriers preserve native precision, epochs and independent infinite endpoints.

use super::{boundary, wrong_type, ScalarAccess, UQAError, UQAValue};
use crate::abi::{
    UQADate, UQAInterval, UQAStatus, UQATimeTz, UQATimestamp, UQA_ERROR_RANGE, UQA_OK,
    UQA_TEMPORAL_FINITE, UQA_TEMPORAL_NEGATIVE_INFINITY, UQA_TEMPORAL_POSITIVE_INFINITY,
};
use uqa_core::{
    memory::ProductionControl, TemporalInfinity, TemporalValue, Value, POSTGRES_EPOCH_DAYS,
};
use uqa_sql::{ast::ColumnType, SQLParam};

fn infinity(kind: u32) -> boundary::Result<Option<TemporalInfinity>> {
    match kind {
        UQA_TEMPORAL_FINITE => Ok(None),
        UQA_TEMPORAL_NEGATIVE_INFINITY => Ok(Some(TemporalInfinity::Negative)),
        UQA_TEMPORAL_POSITIVE_INFINITY => Ok(Some(TemporalInfinity::Positive)),
        _ => Err(UQAError::invalid("unknown temporal kind")),
    }
}

fn kind(value: &TemporalValue) -> u32 {
    match value.infinity() {
        None => UQA_TEMPORAL_FINITE,
        Some(TemporalInfinity::Negative) => UQA_TEMPORAL_NEGATIVE_INFINITY,
        Some(TemporalInfinity::Positive) => UQA_TEMPORAL_POSITIVE_INFINITY,
    }
}

fn reserved(value: u32) -> boundary::Result<()> {
    if value == 0 {
        Ok(())
    } else {
        Err(UQAError::invalid("reserved temporal fields must be zero"))
    }
}

fn publish(
    value: Option<TemporalValue>,
    ty: ColumnType,
    out: &mut *mut UQAValue,
) -> boundary::Result<UQAStatus> {
    let value = value.ok_or_else(|| {
        UQAError::new(
            UQA_ERROR_RANGE,
            "temporal input is outside the SQL type's range",
        )
    })?;
    let control = ProductionControl::uncontrolled();
    let parameter = control.finish(
        SQLParam::typed_scalar(Value::Temporal(value), ty),
        control.empty_reservation(),
    )?;
    *out = Box::into_raw(UQAValue::owned(parameter, None)?);
    Ok(UQA_OK)
}

ffi_fn!(uqa_value_create_date(value: UQADate, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    let value = match infinity(value.kind)? {
        Some(value) => Some(value.date()),
        None => TemporalValue::from_postgres_date(value.days),
    };
    publish(value, ColumnType::Date, out)
});

ffi_fn!(uqa_value_create_time(micros: i64, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    publish(TemporalValue::from_time_micros(micros), ColumnType::Time, out)
});

ffi_fn!(uqa_value_create_timetz(value: UQATimeTz, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    reserved(value.reserved)?;
    publish(TemporalValue::from_time_tz_micros(value.micros, value.offset_seconds), ColumnType::TimeTz, out)
});

macro_rules! timestamp_constructor {
    ($name:ident, $ty:ident, $zoned:expr, $infinite:ident) => {
        ffi_fn!($name(value: UQATimestamp, out: *mut *mut UQAValue) {
            let out = boundary::new_handle(out)?;
            reserved(value.reserved)?;
            let value = match infinity(value.kind)? {
                Some(value) => Some(value.$infinite()),
                None => TemporalValue::from_postgres_timestamp(value.micros, $zoned),
            };
            publish(value, ColumnType::$ty, out)
        });
    };
}
timestamp_constructor!(uqa_value_create_timestamp, Timestamp, false, timestamp);
timestamp_constructor!(
    uqa_value_create_timestamptz,
    TimestampTz,
    true,
    timestamp_tz
);

ffi_fn!(uqa_value_create_interval(value: UQAInterval, out: *mut *mut UQAValue) {
    let out = boundary::new_handle(out)?;
    reserved(value.reserved)?;
    let value = match infinity(value.kind)? {
        Some(value) => Some(value.interval()),
        None => TemporalValue::from_interval_fields(value.months, value.days, value.micros),
    };
    publish(value, ColumnType::Interval, out)
});

macro_rules! temporal_getter {
    ($name:ident, $output:ty, $access:ident, $pattern:pat => $convert:expr) => {
        ffi_fn!($name(value: *const UQAValue, out: *mut $output) {
            let out = boundary::output(out)?;
            let value = boundary::input(value)?;
            let _call = value.session.as_ref().map(|session| session.enter_value()).transpose()?;
            value.check_access(ScalarAccess::$access)?;
            match value.observed()? {
                Value::Temporal($pattern) => { *out = $convert; Ok(UQA_OK) },
                value => Err(wrong_type(value)),
            }
        });
    };
}

fn date(value: &TemporalValue) -> boundary::Result<UQADate> {
    let TemporalValue::Date { days } = value else {
        unreachable!("date accessor");
    };
    Ok(UQADate {
        kind: kind(value),
        days: if value.is_finite() {
            days.checked_sub(POSTGRES_EPOCH_DAYS).ok_or_else(|| {
                UQAError::new(UQA_ERROR_RANGE, "retained date exceeds the C day carrier")
            })?
        } else {
            0
        },
    })
}

fn interval(value: &TemporalValue) -> UQAInterval {
    let TemporalValue::Interval {
        micros,
        days,
        months,
    } = value
    else {
        unreachable!("interval accessor");
    };
    let (micros, days, months) = if value.is_finite() {
        (*micros, *days, *months)
    } else {
        (0, 0, 0)
    };
    UQAInterval {
        micros,
        days,
        months,
        kind: kind(value),
        reserved: 0,
    }
}

temporal_getter!(uqa_value_get_date, UQADate, Date, temporal @ TemporalValue::Date { .. } => date(temporal)?);
temporal_getter!(uqa_value_get_time, i64, Time, TemporalValue::Time { micros } => *micros);
temporal_getter!(uqa_value_get_timetz, UQATimeTz, TimeTz, TemporalValue::TimeTz { micros, offset_seconds } => UQATimeTz { micros: *micros, offset_seconds: *offset_seconds, reserved: 0 });
temporal_getter!(uqa_value_get_timestamp, UQATimestamp, Timestamp, temporal @ TemporalValue::Timestamp { postgres_micros } => UQATimestamp { micros: if temporal.is_finite() { *postgres_micros } else { 0 }, kind: kind(temporal), reserved: 0 });
temporal_getter!(uqa_value_get_timestamptz, UQATimestamp, TimestampTz, temporal @ TemporalValue::TimestampTz { postgres_micros } => UQATimestamp { micros: if temporal.is_finite() { *postgres_micros } else { 0 }, kind: kind(temporal), reserved: 0 });
temporal_getter!(uqa_value_get_interval, UQAInterval, Interval, temporal @ TemporalValue::Interval { .. } => interval(temporal));
