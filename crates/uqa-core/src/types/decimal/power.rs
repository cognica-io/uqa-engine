//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integer exponentiation with `PostgreSQL`'s intermediate numeric precision.

use num_bigint::BigInt;
use num_traits::Zero;

use super::{
    decimal_digit_count, divide_round_away_from_zero, pow10, rounded_decimal_product,
    select_decimal_power_scale, DecimalRepr, DecimalValue, MAX_DISPLAY_SCALE, MAX_INTEGER_DIGITS,
};

const MAX_GROUP_WEIGHT: i32 = i16::MAX as i32;

pub(super) fn decimal_power_integer(
    base: &DecimalValue,
    exponent: i32,
    exponent_scale: u32,
) -> Option<DecimalValue> {
    let approximate_weight = if base.is_zero() {
        0.0
    } else {
        base.approximate_log10_abs()? * f64::from(exponent)
    };
    if approximate_weight > MAX_INTEGER_DIGITS as f64 {
        return None;
    }
    if approximate_weight + 1.0 < -f64::from(MAX_DISPLAY_SCALE) {
        return DecimalValue::from_i64(0).round_to_scale(MAX_DISPLAY_SCALE as i32);
    }
    let result_scale =
        select_decimal_power_scale(approximate_weight, base.display_scale()?, exponent_scale);
    match exponent {
        0 => return DecimalValue::from_i64(1).round_to_scale(result_scale),
        1 => return base.round_to_scale(result_scale),
        -1 => {
            return DecimalValue::from_i64(1)
                .checked_div_to_scale(base, u32::try_from(result_scale).ok()?)
        }
        2 => return rounded_decimal_product(base, base, result_scale),
        _ => {}
    }
    if base.is_zero() {
        return DecimalValue::from_i64(0).round_to_scale(result_scale);
    }

    let mut significant_digits = 1 + result_scale + approximate_weight.trunc() as i32;
    significant_digits += f64::from(exponent).abs().ln().trunc() as i32 + 8;
    let negative_exponent = exponent.is_negative();
    let mut mask = exponent.unsigned_abs();
    let mut factor = PowerIntermediate::from_decimal(base)?;
    let mut result = if mask & 1 == 1 {
        factor.clone()
    } else {
        PowerIntermediate::one()
    };
    while {
        mask >>= 1;
        mask > 0
    } {
        let factor_scale = significant_digits
            .saturating_sub(factor.group_weight()?.saturating_mul(8))
            .min(i32::try_from(factor.scale.saturating_mul(2)).unwrap_or(i32::MAX))
            .max(0);
        factor = factor.multiply_rounded(&factor, factor_scale)?;
        if mask & 1 == 1 {
            let product_scale = significant_digits
                .saturating_sub(
                    factor
                        .group_weight()?
                        .saturating_add(result.group_weight()?)
                        .saturating_mul(4),
                )
                .min(i32::try_from(factor.scale.saturating_add(result.scale)).unwrap_or(i32::MAX))
                .max(0);
            result = factor.multiply_rounded(&result, product_scale)?;
        }
        if factor.group_weight()? > MAX_GROUP_WEIGHT || result.group_weight()? > MAX_GROUP_WEIGHT {
            return if negative_exponent {
                DecimalValue::from_i64(0).round_to_scale(result_scale)
            } else {
                None
            };
        }
    }
    let result_scale = u32::try_from(result_scale).ok()?;
    if negative_exponent {
        result.reciprocal(result_scale)
    } else {
        result.into_decimal(result_scale)
    }
}

#[derive(Clone)]
struct PowerIntermediate {
    coefficient: BigInt,
    scale: u32,
}

impl PowerIntermediate {
    fn from_decimal(value: &DecimalValue) -> Option<Self> {
        let DecimalRepr::Finite { coefficient, scale } = value.repr() else {
            return None;
        };
        Some(Self {
            coefficient: coefficient.clone(),
            scale: *scale,
        })
    }

    fn one() -> Self {
        Self {
            coefficient: BigInt::from(1_u8),
            scale: 0,
        }
    }

    fn group_weight(&self) -> Option<i32> {
        if self.coefficient.is_zero() {
            return Some(0);
        }
        i32::try_from(decimal_digit_count(&self.coefficient))
            .ok()?
            .checked_sub(i32::try_from(self.scale).ok()?)?
            .checked_sub(1)
            .map(|weight| weight.div_euclid(4))
    }

    fn multiply_rounded(&self, rhs: &Self, result_scale: i32) -> Option<Self> {
        let source_scale = self.scale.checked_add(rhs.scale)?;
        let result_scale = u32::try_from(result_scale).ok()?;
        let coefficient = &self.coefficient * &rhs.coefficient;
        let coefficient = quantize_coefficient(coefficient, source_scale, result_scale)?;
        Some(Self {
            coefficient,
            scale: result_scale,
        })
    }

    fn reciprocal(self, result_scale: u32) -> Option<DecimalValue> {
        if self.coefficient.is_zero() {
            return None;
        }
        let numerator = pow10(self.scale.checked_add(result_scale)?);
        let coefficient = divide_round_away_from_zero(&numerator, &self.coefficient)?;
        DecimalValue::finite(coefficient, result_scale)
    }

    fn into_decimal(self, result_scale: u32) -> Option<DecimalValue> {
        let coefficient = quantize_coefficient(self.coefficient, self.scale, result_scale)?;
        DecimalValue::finite(coefficient, result_scale)
    }
}

fn quantize_coefficient(
    coefficient: BigInt,
    source_scale: u32,
    result_scale: u32,
) -> Option<BigInt> {
    if result_scale < source_scale {
        divide_round_away_from_zero(&coefficient, &pow10(source_scale - result_scale))
    } else {
        Some(coefficient * pow10(result_scale - source_scale))
    }
}
