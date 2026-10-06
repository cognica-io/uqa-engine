//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Extraction admits exact numeric coefficients and arithmetic through the caller's production control.

use uqa_core::{
    memory::{Produced, ProductionControl},
    DecimalValue, Value,
};

use crate::error::{Result, SQLError};

use super::{MICROS_PER_DAY, MICROS_PER_SECOND};

pub(super) enum Part {
    Integer(i64),
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
        if !as_numeric {
            let float = match self {
                Self::Integer(integer) => integer as f64,
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
            Self::Integer(integer) => DecimalValue::from_i64_with_control(integer, control)?,
            Self::Scaled {
                coefficient, scale, ..
            } => scaled(coefficient, scale, control)?,
            Self::Julian { day, micros } => julian(day, micros, control)?,
        };
        let (value, memory) = decimal.into_parts();
        Ok(control.finish(Value::Decimal(value), memory)?)
    }
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
