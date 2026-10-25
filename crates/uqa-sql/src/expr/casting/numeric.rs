//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Numeric coercions selected from resolved SQL types, without spelling or parsing their names again.

use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    DecimalValue, Value,
};

/// An immutable builtin numeric conversion selected from resolved source and destination identities.
#[derive(Clone, Copy, Debug)]
pub enum NumericCast {
    SmallInteger,
    Integer,
    BigInteger,
    Real,
    DoublePrecision,
    Numeric {
        precision: Option<u32>,
        scale: Option<i32>,
    },
}

impl NumericCast {
    /// Select only builtin numeric identities. Domains, OID aliases, containers and catalog-defined types retain their distinct coercion paths.
    pub fn select(source: &ColumnType, target: &ColumnType) -> Option<Self> {
        if !matches!(
            source,
            ColumnType::SmallInteger
                | ColumnType::Integer
                | ColumnType::BigInteger
                | ColumnType::Real
                | ColumnType::DoublePrecision
                | ColumnType::Numeric { .. }
        ) {
            return None;
        }
        Some(match target {
            ColumnType::SmallInteger => Self::SmallInteger,
            ColumnType::Integer => Self::Integer,
            ColumnType::BigInteger => Self::BigInteger,
            ColumnType::Real => Self::Real,
            ColumnType::DoublePrecision => Self::DoublePrecision,
            ColumnType::Numeric { precision, scale } => Self::Numeric {
                precision: *precision,
                scale: *scale,
            },
            _ => return None,
        })
    }

    /// Execute a retained conversion with the invoking catalog hook. Type resolution is already complete, but domain adapters still observe each value before the builtin kernel.
    pub fn evaluate_with_catalog(
        self,
        value: &Value,
        source_name: &str,
        engine: Option<&dyn super::super::EngineHook>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        control.check()?;
        let target = self.target_type();
        if let Some(engine) = engine {
            if let Some(output) =
                engine.cast_domain_with_control(value, Some(source_name), &target, control)?
            {
                return Ok(output);
            }
        }
        self.evaluate(value, control)
    }

    pub(crate) fn target_type(self) -> ColumnType {
        match self {
            Self::SmallInteger => ColumnType::SmallInteger,
            Self::Integer => ColumnType::Integer,
            Self::BigInteger => ColumnType::BigInteger,
            Self::Real => ColumnType::Real,
            Self::DoublePrecision => ColumnType::DoublePrecision,
            Self::Numeric { precision, scale } => ColumnType::Numeric { precision, scale },
        }
    }

    /// Execute after the caller's catalog/domain checks, using the existing numeric conversion kernels and the current invocation's resource control.
    pub fn evaluate(
        self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        control.check()?;
        if let Value::Datum(datum) = value {
            return self.evaluate(
                &*super::super::datums::read_with_control(datum, control)?,
                control,
            );
        }
        let converted = match (self, value) {
            (_, Value::Null) => Value::Null,
            (Self::SmallInteger, _) => super::cast_integer(value, "smallint", control)?,
            (Self::Integer, _) => super::cast_integer(value, "integer", control)?,
            (Self::BigInteger, _) => super::cast_integer(value, "bigint", control)?,
            (Self::Real, _) => Value::Float(super::super::floating::to_float_with_control(
                value,
                super::super::FloatWidth::Real,
                control,
            )?),
            (Self::DoublePrecision, _) => {
                Value::Float(super::super::floating::to_float_with_control(
                    value,
                    super::super::FloatWidth::DoublePrecision,
                    control,
                )?)
            }
            (Self::Numeric { precision, scale }, _) => {
                return finish(
                    super::super::conversion::to_decimal_with_control(value, control)?,
                    precision.map(|precision| (precision, scale.unwrap_or(0))),
                    control,
                );
            }
        };
        Ok(control.finish(converted, control.empty_reservation())?)
    }
}

pub(super) fn finish(
    value: Produced<DecimalValue>,
    modifier: Option<(u32, i32)>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let value = if let Some((precision, scale)) = modifier {
        let rounded = value
            .round_to_scale_with_control(scale, control)?
            .ok_or_else(|| super::out_of_range("numeric"))?;
        if !rounded.fits_precision_with_control(precision, scale, control)? {
            return Err(crate::assignment::conversion::numeric_field_overflow(
                precision, scale,
            ));
        }
        rounded
    } else {
        value
    };
    let (value, memory) = value.into_parts();
    Ok(control.finish(Value::Decimal(value), memory)?)
}

#[cfg(test)]
mod tests;
