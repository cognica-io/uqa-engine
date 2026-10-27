//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Extraction admits exact numeric coefficients and arithmetic through the caller's production control.

use uqa_core::{
    memory::{Produced, ProductionControl},
    DecimalValue, TemporalInfinity, Value,
};

use crate::error::{Result, SQLError};

use super::{MICROS_PER_DAY, MICROS_PER_SECOND};

pub(super) enum Part {
    Null,
    Infinity(TemporalInfinity),
    Integer(i64),
    TimestampEpoch(i64),
    Scaled {
        coefficient: i128,
        scale: u32,
        float: f64,
    },
    Julian {
        day: i64,
        micros: i64,
    },
}

impl Part {
    pub(super) fn finish(
        self,
        as_numeric: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>> {
        if matches!(self, Self::Null) {
            return Ok(control.finish(Value::Null, control.empty_reservation())?);
        }
        if !as_numeric {
            let float = match self {
                Self::Null => unreachable!("NULL part returned above"),
                Self::Infinity(TemporalInfinity::Negative) => f64::NEG_INFINITY,
                Self::Infinity(TemporalInfinity::Positive) => f64::INFINITY,
                Self::Integer(integer) => integer as f64,
                Self::TimestampEpoch(micros) => {
                    let epoch = uqa_core::POSTGRES_EPOCH_MICROS;
                    if micros < i64::MAX - epoch {
                        (micros + epoch) as f64 / 1_000_000.0
                    } else {
                        (micros as f64 + epoch as f64) / 1_000_000.0
                    }
                }
                Self::Scaled { float, .. } => float,
                Self::Julian { day, micros } => {
                    day as f64
                        + ((micros / MICROS_PER_SECOND) as f64
                            + (micros % MICROS_PER_SECOND) as f64 / 1_000_000.0)
                            / 86_400.0
                }
            };
            return Ok(control.finish(Value::Float(float), control.empty_reservation())?);
        }
        let decimal = match self {
            Self::Null => unreachable!("NULL part returned above"),
            Self::Infinity(infinity) => {
                DecimalValue::parse_with_control(infinity.as_str(), control)?
                    .expect("numeric infinity input")
            }
            Self::Integer(integer) => DecimalValue::from_i64_with_control(integer, control)?,
            Self::TimestampEpoch(micros) => timestamp_epoch(micros, control)?,
            Self::Scaled {
                coefficient, scale, ..
            } => scaled(coefficient, scale, control)?,
            Self::Julian { day, micros } => julian(day, micros, control)?,
        };
        let (value, memory) = decimal.into_parts();
        Ok(control.finish(Value::Decimal(value), memory)?)
    }
}

fn timestamp_epoch(micros: i64, control: &ProductionControl<'_>) -> Result<Produced<DecimalValue>> {
    let epoch = uqa_core::POSTGRES_EPOCH_MICROS;
    if micros < i64::MAX - epoch {
        return scaled(i128::from(micros + epoch), 6, control);
    }
    // PostgreSQL's overflowing integer subtraction takes the numeric division path, including its selected division scale before rounding to six places.
    let stamp = DecimalValue::from_i64_with_control(micros, control)?;
    let epoch = DecimalValue::from_i64_with_control(epoch, control)?;
    let numerator = stamp
        .checked_add_with_control(&epoch, control)?
        .ok_or_else(|| SQLError::Internal("timestamp epoch addition failed".into()))?;
    let denominator = DecimalValue::from_i64_with_control(MICROS_PER_SECOND, control)?;
    let quotient = numerator
        .checked_div_postgres_with_control(&denominator, control)?
        .ok_or_else(|| SQLError::Internal("timestamp epoch division failed".into()))?;
    quotient
        .round_to_scale_with_control(6, control)?
        .ok_or_else(|| SQLError::Internal("timestamp epoch rounding failed".into()))
}

fn scaled(
    coefficient: i128,
    scale: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<DecimalValue>> {
    let sign = if coefficient < 0 { "-" } else { "" };
    let magnitude = coefficient.unsigned_abs();
    let divisor = 10_u128.pow(scale);
    let integer = magnitude / divisor;
    let fraction = magnitude % divisor;
    let text = control.format(format_args!(
        "{sign}{integer}.{fraction:0width$}",
        width = scale as usize,
    ))?;
    DecimalValue::parse_with_control(&text, control)?.ok_or_else(|| {
        SQLError::Internal("finite temporal coefficient is not a numeric value".into())
    })
}

fn julian(
    day: i64,
    micros: i64,
    control: &ProductionControl<'_>,
) -> Result<Produced<DecimalValue>> {
    let numerator = DecimalValue::from_i64_with_control(micros, control)?;
    let denominator = DecimalValue::from_i64_with_control(MICROS_PER_DAY, control)?;
    let fraction = numerator
        .checked_div_postgres_with_control(&denominator, control)?
        .ok_or_else(|| SQLError::Internal("finite Julian day division failed".into()))?;
    drop(numerator);
    drop(denominator);
    let integer = DecimalValue::from_i64_with_control(day, control)?;
    fraction
        .checked_add_with_control(&integer, control)?
        .ok_or_else(|| SQLError::Internal("finite Julian day addition failed".into()))
}
