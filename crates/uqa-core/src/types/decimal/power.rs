//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integer exponentiation with `PostgreSQL`'s intermediate numeric precision.

use num_bigint::BigInt;
use num_traits::Zero;

use super::arithmetic_control::{divide_rounded, finite, Calculation, Failure};
use super::transcendental::{round, rounded_decimal_product, select_decimal_power_scale};
use super::{
    coefficient, format_budgeted::coefficient_digits_with_control, DecimalRepr, DecimalValue,
    MAX_DISPLAY_SCALE, MAX_INTEGER_DIGITS,
};
use crate::memory::{Produced, ProductionControl};

const MAX_GROUP_WEIGHT: i32 = i16::MAX as i32;

#[allow(
    clippy::too_many_lines,
    reason = "the existing integer exponentiation keeps scale and reciprocal selection together"
)]
pub(super) fn decimal_power_integer(
    base: &DecimalValue,
    exponent: i32,
    exponent_scale: u32,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<DecimalValue>> {
    control.check()?;
    let approximate_weight = if base.is_zero() {
        0.0
    } else {
        base.approximate_log10_abs_with_control(control)? * f64::from(exponent)
    };
    if approximate_weight > MAX_INTEGER_DIGITS as f64 {
        return Err(Failure::Invalid);
    }
    if approximate_weight + 1.0 < -f64::from(MAX_DISPLAY_SCALE) {
        return round(
            &*DecimalValue::from_i64_with_control(0, control)?,
            MAX_DISPLAY_SCALE as i32,
            control,
        );
    }
    let result_scale = select_decimal_power_scale(
        approximate_weight,
        base.display_scale().ok_or(Failure::Invalid)?,
        exponent_scale,
    );
    match exponent {
        0 => {
            return round(
                &*DecimalValue::from_i64_with_control(1, control)?,
                result_scale,
                control,
            )
        }
        1 => return round(base, result_scale, control),
        -1 => {
            return DecimalValue::from_i64_with_control(1, control)?
                .checked_div_to_scale_with_control(
                    base,
                    u32::try_from(result_scale).map_err(|_| Failure::Invalid)?,
                    control,
                )?
                .ok_or(Failure::Invalid)
        }
        2 => return rounded_decimal_product(base, base, result_scale, control),
        _ => (),
    }
    if base.is_zero() {
        return round(
            &*DecimalValue::from_i64_with_control(0, control)?,
            result_scale,
            control,
        );
    }
    let mut significant_digits = 1 + result_scale + approximate_weight.trunc() as i32;
    significant_digits += f64::from(exponent).abs().ln().trunc() as i32 + 8;
    let negative_exponent = exponent.is_negative();
    let mut mask = exponent.unsigned_abs();
    let mut factor = PowerIntermediate::from_decimal(base, control)?;
    let mut result = if mask & 1 == 1 {
        factor.clone_with_control(control)?
    } else {
        PowerIntermediate::one(control)?
    };
    while {
        mask >>= 1;
        mask > 0
    } {
        control.check()?;
        let factor_scale = significant_digits
            .saturating_sub(factor.group_weight(control)?.saturating_mul(8))
            .min(i32::try_from(factor.scale.saturating_mul(2)).unwrap_or(i32::MAX))
            .max(0);
        factor = factor.multiply_rounded(&factor, factor_scale, control)?;
        if mask & 1 == 1 {
            let product_scale = significant_digits
                .saturating_sub(
                    factor
                        .group_weight(control)?
                        .saturating_add(result.group_weight(control)?)
                        .saturating_mul(4),
                )
                .min(i32::try_from(factor.scale.saturating_add(result.scale)).unwrap_or(i32::MAX))
                .max(0);
            result = factor.multiply_rounded(&result, product_scale, control)?;
        }
        if factor.group_weight(control)? > MAX_GROUP_WEIGHT
            || result.group_weight(control)? > MAX_GROUP_WEIGHT
        {
            return if negative_exponent {
                round(
                    &*DecimalValue::from_i64_with_control(0, control)?,
                    result_scale,
                    control,
                )
            } else {
                Err(Failure::Invalid)
            };
        }
    }
    let result_scale = u32::try_from(result_scale).map_err(|_| Failure::Invalid)?;
    if negative_exponent {
        result.reciprocal(result_scale, control)
    } else {
        result.into_decimal(result_scale, control)
    }
}

struct PowerIntermediate {
    coefficient: Produced<BigInt>,
    scale: u32,
}

impl PowerIntermediate {
    fn from_decimal(value: &DecimalValue, control: &ProductionControl<'_>) -> Calculation<Self> {
        let DecimalRepr::Finite { coefficient, scale } = value.repr() else {
            return Err(Failure::Invalid);
        };
        Ok(Self {
            coefficient: coefficient::clone(coefficient, control)?,
            scale: *scale,
        })
    }

    fn one(control: &ProductionControl<'_>) -> Calculation<Self> {
        Ok(Self {
            coefficient: coefficient::from_i64(1, control)?,
            scale: 0,
        })
    }

    fn clone_with_control(&self, control: &ProductionControl<'_>) -> Calculation<Self> {
        Ok(Self {
            coefficient: coefficient::clone(&self.coefficient, control)?,
            scale: self.scale,
        })
    }

    fn group_weight(&self, control: &ProductionControl<'_>) -> Calculation<i32> {
        if self.coefficient.is_zero() {
            return Ok(0);
        }
        let digits = coefficient_digits_with_control(&self.coefficient, control)?.len();
        i32::try_from(digits)
            .map_err(|_| Failure::Invalid)?
            .checked_sub(i32::try_from(self.scale).map_err(|_| Failure::Invalid)?)
            .and_then(|weight| weight.checked_sub(1))
            .map(|weight| weight.div_euclid(4))
            .ok_or(Failure::Invalid)
    }

    fn multiply_rounded(
        &self,
        rhs: &Self,
        result_scale: i32,
        control: &ProductionControl<'_>,
    ) -> Calculation<Self> {
        let source_scale = self.scale.checked_add(rhs.scale).ok_or(Failure::Invalid)?;
        let result_scale = u32::try_from(result_scale).map_err(|_| Failure::Invalid)?;
        let coefficient = coefficient::multiply(&self.coefficient, &rhs.coefficient, control)?;
        let coefficient = quantize_coefficient(coefficient, source_scale, result_scale, control)?;
        Ok(Self {
            coefficient,
            scale: result_scale,
        })
    }

    fn reciprocal(
        self,
        result_scale: u32,
        control: &ProductionControl<'_>,
    ) -> Calculation<Produced<DecimalValue>> {
        if self.coefficient.is_zero() {
            return Err(Failure::Invalid);
        }
        let numerator = coefficient::power_of_ten(
            self.scale
                .checked_add(result_scale)
                .ok_or(Failure::Invalid)?,
            control,
        )?;
        let coefficient = divide_rounded(&numerator, &self.coefficient, control)?;
        finite(coefficient, result_scale, control)
    }

    fn into_decimal(
        self,
        result_scale: u32,
        control: &ProductionControl<'_>,
    ) -> Calculation<Produced<DecimalValue>> {
        let coefficient =
            quantize_coefficient(self.coefficient, self.scale, result_scale, control)?;
        finite(coefficient, result_scale, control)
    }
}

fn quantize_coefficient(
    coefficient: Produced<BigInt>,
    source_scale: u32,
    result_scale: u32,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<BigInt>> {
    control.check()?;
    if result_scale == source_scale {
        return Ok(coefficient);
    }
    if result_scale < source_scale {
        let divisor = coefficient::power_of_ten(source_scale - result_scale, control)?;
        divide_rounded(&coefficient, &divisor, control)
    } else {
        let power = coefficient::power_of_ten(result_scale - source_scale, control)?;
        Ok(coefficient::multiply(&coefficient, &power, control)?)
    }
}
