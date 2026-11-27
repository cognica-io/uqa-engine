//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transition data shared by grouped, projected and partial aggregates.

use super::{value_as_f64, DecimalValue, NumericInputKind, SQLError, Value};
use uqa_core::TemporalInfinity;
use uqa_sql::expr::{
    enums::{EnumComparisonState, EnumLabelCatalog},
    IntervalFields,
};

#[derive(Default)]
pub(super) struct SumState {
    pub(super) count: u64,
    pub(super) sum: f64,
    pub(super) integer_sum: i128,
    pub(super) decimal_sum: Option<DecimalValue>,
    pub(super) numeric_inputs: NumericInputKind,
    pub(super) interval_sum: Option<IntervalFields>,
    pub(super) interval_positive_infinity: u64,
    pub(super) interval_negative_infinity: u64,
    pub(super) real: bool,
}

pub(super) fn increment(count: &mut u64) -> Result<(), SQLError> {
    *count = count
        .checked_add(1)
        .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
    Ok(())
}

impl SumState {
    pub(super) fn add_integer(&mut self, n: i64) -> Result<(), SQLError> {
        self.integer_sum = self
            .integer_sum
            .checked_add(i128::from(n))
            .ok_or_else(|| SQLError::TypeMismatch("integer aggregate overflow".into()))?;
        if self.numeric_inputs.has_decimal() {
            let next = DecimalValue::from_i64(n);
            self.decimal_sum = Some(
                self.decimal_sum
                    .as_ref()
                    .and_then(|sum| sum.checked_add(&next))
                    .ok_or_else(|| SQLError::TypeMismatch("decimal aggregate overflow".into()))?,
            );
        }
        if self.numeric_inputs.has_float() {
            self.sum += n as f64;
        }
        Ok(())
    }

    pub(super) fn observe(&mut self, value: &Value) -> Result<(), SQLError> {
        if let Value::Temporal(temporal) = value {
            let interval = IntervalFields::of(temporal).ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "SUM/AVG requires a numeric or interval value, got {value:?}"
                ))
            })?;
            if let Some(infinity) = temporal.infinity() {
                let count = match infinity {
                    TemporalInfinity::Positive => &mut self.interval_positive_infinity,
                    TemporalInfinity::Negative => &mut self.interval_negative_infinity,
                };
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
                return Ok(());
            }
            self.interval_sum = Some(
                self.interval_sum
                    .unwrap_or(IntervalFields::ZERO)
                    .plus(interval)?,
            );
            return Ok(());
        }
        if !matches!(value, Value::Int(_) | Value::Float(_) | Value::Decimal(_)) {
            return Err(SQLError::TypeMismatch(format!(
                "SUM/AVG requires a numeric or interval value, got {value:?}"
            )));
        }
        match value {
            Value::Int(n) => self.add_integer(*n)?,
            Value::Decimal(d) => {
                let next = match &self.decimal_sum {
                    Some(sum) => sum.checked_add(d),
                    None if self.integer_sum == 0 => Some(d.clone()),
                    None => {
                        DecimalValue::from_i128(self.integer_sum).and_then(|sum| sum.checked_add(d))
                    }
                }
                .ok_or_else(|| SQLError::TypeMismatch("decimal aggregate overflow".into()))?;
                self.decimal_sum = Some(next);
                if self.numeric_inputs.has_float() {
                    self.sum += d.to_f64().ok_or_else(|| {
                        SQLError::TypeMismatch("decimal aggregate does not fit float".into())
                    })?;
                }
                self.numeric_inputs.observe_decimal();
            }
            Value::Float(value) => {
                if !self.numeric_inputs.has_float() {
                    // Exact integer/decimal aggregates do not maintain a shadow
                    // floating total. Convert the accumulated total only if a
                    // floating input actually makes it necessary.
                    self.sum = if self.numeric_inputs.has_decimal() {
                        self.decimal_sum
                            .as_ref()
                            .and_then(DecimalValue::to_f64)
                            .ok_or_else(|| {
                                SQLError::TypeMismatch(
                                    "decimal aggregate does not fit float".into(),
                                )
                            })?
                    } else {
                        self.integer_sum as f64
                    };
                }
                self.sum = if self.count == 1 {
                    *value
                } else {
                    self.combine_float_sum(self.sum, *value)?
                };
                self.numeric_inputs.observe_float();
            }
            _ => {
                return Err(SQLError::TypeMismatch(format!(
                    "SUM/AVG requires a numeric or interval value, got {value:?}"
                )))
            }
        }
        Ok(())
    }

    /// Select the interval result after all transitions, preserving finite overflow and later input errors even when both signs of infinity have been seen.
    pub(super) fn interval_result(&self) -> Result<Option<IntervalFields>, SQLError> {
        let infinity = match (
            self.interval_positive_infinity != 0,
            self.interval_negative_infinity != 0,
        ) {
            (true, true) => {
                return Err(SQLError::Routine {
                    sqlstate: "22008".into(),
                    message: "interval out of range".into(),
                })
            }
            (true, false) => Some(TemporalInfinity::Positive),
            (false, true) => Some(TemporalInfinity::Negative),
            (false, false) => None,
        };
        Ok(infinity.map_or(self.interval_sum, |value| {
            IntervalFields::of(&value.interval())
        }))
    }

    pub(super) fn combine_float_sum(&self, left: f64, right: f64) -> Result<f64, SQLError> {
        use uqa_sql::expr::{eval_float_arithmetic, FloatWidth};
        let width = if self.real {
            FloatWidth::Real
        } else {
            FloatWidth::DoublePrecision
        };
        match eval_float_arithmetic(
            uqa_sql::ast::BinaryOp::Add,
            &Value::Float(left),
            &Value::Float(right),
            width,
        )? {
            Value::Float(value) => Ok(value),
            _ => Err(SQLError::Internal(
                "floating sum lost its numeric carrier".into(),
            )),
        }
    }
}

#[derive(Default)]
pub(super) struct GenericState {
    pub(super) sum: SumState,
    pub(super) min: Option<Value>,
    pub(super) max: Option<Value>,
    pub(super) bool_and: Option<bool>,
    pub(super) bool_or: Option<bool>,
}

impl GenericState {
    pub(super) fn retained_bytes(&self) -> usize {
        use super::retained_size::payload_bytes;
        size_of::<Self>()
            .saturating_add(
                self.sum
                    .decimal_sum
                    .as_ref()
                    .map_or(0, DecimalValue::retained_bytes),
            )
            .saturating_add(self.min.as_ref().map_or(0, payload_bytes))
            .saturating_add(self.max.as_ref().map_or(0, payload_bytes))
    }

    pub(super) fn observe(
        &mut self,
        value: &Value,
        enums: Option<&dyn EnumLabelCatalog>,
        comparison: Option<&EnumComparisonState>,
    ) -> Result<(), SQLError> {
        increment(&mut self.sum.count)?;
        if matches!(value, Value::Int(_) | Value::Float(_) | Value::Decimal(_)) {
            self.sum.observe(value)?;
        }
        observe_extreme(
            &mut self.min,
            value,
            std::cmp::Ordering::Greater,
            enums,
            comparison,
        )?;
        observe_extreme(
            &mut self.max,
            value,
            std::cmp::Ordering::Less,
            enums,
            comparison,
        )?;
        if let Value::Bool(value) = value {
            self.bool_and = Some(self.bool_and.unwrap_or(true) && *value);
            self.bool_or = Some(self.bool_or.unwrap_or(false) || *value);
        }
        Ok(())
    }
}

pub(super) fn observe_extreme(
    retained: &mut Option<Value>,
    value: &Value,
    replace_on: std::cmp::Ordering,
    enums: Option<&dyn EnumLabelCatalog>,
    comparison: Option<&EnumComparisonState>,
) -> Result<(), SQLError> {
    if let Some(current) = retained {
        if super::ordering::compare_extrema_with_catalog(current, value, enums, comparison)?
            != replace_on
        {
            return Ok(());
        }
    }
    *retained = Some(value.clone());
    Ok(())
}

/// Floating moments and exact decimal deviations for one statistical aggregate.
pub(super) struct StatisticsState {
    pub(super) count: u64,
    pub(super) mean: f64,
    pub(super) m2: f64,
    pub(super) moments_valid: bool,
    pub(super) has_float: bool,
    pub(super) nonzero_deviation: bool,
    pub(super) origin: Option<DecimalValue>,
    pub(super) sum: Option<DecimalValue>,
    pub(super) sum_squares: Option<DecimalValue>,
}

impl Default for StatisticsState {
    fn default() -> Self {
        Self {
            count: 0,
            mean: 0.0,
            m2: 0.0,
            moments_valid: true,
            has_float: false,
            nonzero_deviation: false,
            origin: None,
            sum: None,
            sum_squares: None,
        }
    }
}

impl StatisticsState {
    pub(super) fn observe(&mut self, value: &Value) -> Result<(), SQLError> {
        let previous_count = self.count;
        let next_count = previous_count
            .checked_add(1)
            .ok_or_else(|| SQLError::TypeMismatch("statistical aggregate count overflow".into()))?;
        if matches!(value, Value::Float(_)) {
            if !self.moments_valid {
                return Err(SQLError::TypeMismatch(
                    "numeric statistical state does not fit double precision".into(),
                ));
            }
            self.has_float = true;
            self.observe_float_statistic(value_as_f64(value)?, next_count);
        } else {
            let decimal = match value {
                Value::Int(value) => DecimalValue::from_i64(*value),
                Value::Decimal(value) => value.clone(),
                other => {
                    return Err(SQLError::TypeMismatch(format!(
                        "statistical aggregate requires a numeric value, got {other:?}"
                    )))
                }
            };
            if self.moments_valid {
                if let Some(float) = decimal.to_f64() {
                    self.observe_float_statistic(float, next_count);
                } else {
                    self.moments_valid = false;
                    if self.has_float {
                        return Err(SQLError::TypeMismatch(
                            "numeric statistical input does not fit double precision".into(),
                        ));
                    }
                }
            }
            if decimal.is_nan() || decimal.is_infinite() {
                let nan = decimal.checked_sub(&decimal).ok_or_else(|| {
                    SQLError::Internal("numeric special value did not produce NaN".into())
                })?;
                self.origin.get_or_insert_with(|| DecimalValue::from_i64(0));
                self.sum = Some(nan.clone());
                self.sum_squares = Some(nan);
                self.nonzero_deviation = true;
                self.count = next_count;
                return Ok(());
            }
            if self.sum_squares.as_ref().is_some_and(DecimalValue::is_nan) {
                self.count = next_count;
                return Ok(());
            }
            let origin = if let Some(origin) = &self.origin {
                origin.clone()
            } else {
                let origin = decimal.clone();
                self.origin = Some(origin.clone());
                origin
            };
            let deviation = decimal.checked_sub(&origin).ok_or_else(|| {
                SQLError::TypeMismatch("numeric statistical deviation overflow".into())
            })?;
            self.nonzero_deviation |= !deviation.is_zero();
            let square = deviation.checked_mul(&deviation).ok_or_else(|| {
                SQLError::TypeMismatch("numeric statistical deviation square overflow".into())
            })?;
            self.sum = Some(match self.sum.take() {
                Some(sum) => sum.checked_add(&deviation).ok_or_else(|| {
                    SQLError::TypeMismatch("numeric statistical deviation sum overflow".into())
                })?,
                None => deviation,
            });
            self.sum_squares = Some(match self.sum_squares.take() {
                Some(sum) => sum.checked_add(&square).ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "numeric statistical deviation square sum overflow".into(),
                    )
                })?,
                None => square,
            });
        }
        self.count = next_count;
        Ok(())
    }

    fn observe_float_statistic(&mut self, value: f64, count: u64) {
        let delta = value - self.mean;
        self.mean += delta / count as f64;
        let delta_after = value - self.mean;
        self.m2 += delta * delta_after;
    }
}
