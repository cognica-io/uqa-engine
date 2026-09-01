//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decimal square root, logarithm, exponential, and power operations.

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

use super::arithmetic::numeric_group_head;
use super::{
    decimal_digit_count, pow10, power, DecimalRepr, DecimalValue, MAX_DISPLAY_SCALE,
    MAX_FRACTIONAL_DIGITS, MAX_RESULT_SCALE,
};

impl DecimalValue {
    /// Square root rounded to an explicit display scale. The calculation is performed entirely with integers, so it does not lose numeric digits through a binary floating-point conversion.
    pub fn sqrt_to_scale(&self, result_scale: u32) -> Option<Self> {
        if result_scale > MAX_FRACTIONAL_DIGITS || self.is_negative_infinity() {
            return None;
        }
        if self.is_nan() {
            return Some(Self::nan());
        }
        if self.is_positive_infinity() {
            return Some(Self::positive_infinity());
        }
        let DecimalRepr::Finite { coefficient, scale } = self.repr() else {
            unreachable!("special numeric handled above")
        };
        if coefficient.is_negative() {
            return None;
        }
        let exponent = i64::from(result_scale)
            .checked_mul(2)?
            .checked_sub(i64::from(*scale))?;
        let (radicand, denominator) = if exponent >= 0 {
            (
                coefficient * pow10(u32::try_from(exponent).ok()?),
                BigInt::from(1_u8),
            )
        } else {
            (
                coefficient.clone(),
                pow10(u32::try_from(exponent.checked_neg()?).ok()?),
            )
        };
        let mut root = integer_sqrt_floor(&(&radicand / &denominator));
        let midpoint = (&root * 2_u8) + 1_u8;
        if &radicand * 4_u8 >= &denominator * &midpoint * &midpoint {
            root += 1_u8;
        }
        Self::finite(root, result_scale)
    }

    /// Raise one `NUMERIC` value to another using `PostgreSQL`'s result-scale selection and decimal `ln`/`exp` algorithms. Domain errors are left to the SQL layer so it can attach `PostgreSQL`'s required SQLSTATE.
    pub fn checked_pow_postgres(&self, exponent: &Self) -> Option<Self> {
        decimal_power(self, exponent)
    }

    pub(super) fn approximate_log10_abs(&self) -> Option<f64> {
        let DecimalRepr::Finite { coefficient, scale } = self.repr() else {
            return None;
        };
        if coefficient.is_zero() {
            return None;
        }
        let digits = coefficient.abs().to_str_radix(10);
        let take = digits.len().min(16);
        let leading = digits[..take].parse::<f64>().ok()?;
        let mantissa = leading / 10_f64.powi(i32::try_from(take).ok()?.saturating_sub(1));
        let weight = i32::try_from(digits.len())
            .ok()?
            .checked_sub(i32::try_from(*scale).ok()?)?
            .checked_sub(1)?;
        Some(f64::from(weight) + mantissa.log10())
    }
}

fn integer_sqrt_floor(value: &BigInt) -> BigInt {
    debug_assert!(!value.is_negative());
    if value <= &BigInt::from(1_u8) {
        return value.clone();
    }
    let digits = decimal_digit_count(value);
    let initial_power = u32::try_from(digits.div_ceil(2)).unwrap_or(u32::MAX);
    let mut estimate = pow10(initial_power);
    loop {
        let next = (&estimate + value / &estimate) / 2_u8;
        if next >= estimate {
            return estimate;
        }
        estimate = next;
    }
}

fn decimal_power(base: &DecimalValue, exponent: &DecimalValue) -> Option<DecimalValue> {
    let zero = DecimalValue::from_i64(0);
    let one = DecimalValue::from_i64(1);

    if base.is_nan() {
        return Some(if exponent.is_zero() {
            one
        } else {
            DecimalValue::nan()
        });
    }
    if exponent.is_nan() {
        return Some(if base == &one {
            one
        } else {
            DecimalValue::nan()
        });
    }
    if base.is_zero() && exponent.is_negative() {
        return None;
    }
    if base.is_negative() && !exponent.is_integral() {
        return None;
    }

    if exponent.is_infinite() {
        let absolute = base.abs();
        if absolute == one {
            return Some(one);
        }
        let grows = (absolute > one) == exponent.is_positive_infinity();
        return Some(if grows {
            DecimalValue::positive_infinity()
        } else {
            zero
        });
    }
    if base.is_positive_infinity() {
        return Some(if exponent.is_zero() {
            one
        } else if exponent.is_negative() {
            zero
        } else {
            DecimalValue::positive_infinity()
        });
    }
    if base.is_negative_infinity() {
        if exponent.is_zero() {
            return Some(one);
        }
        if exponent.is_negative() {
            return Some(zero);
        }
        return Some(if decimal_integral_is_odd(exponent)? {
            DecimalValue::negative_infinity()
        } else {
            DecimalValue::positive_infinity()
        });
    }

    if let Some(integer) = decimal_integral_i32(exponent) {
        return power::decimal_power_integer(base, integer, exponent.display_scale()?);
    }

    if base.is_zero() {
        return zero.round_to_scale(16);
    }

    let negative_result = base.is_negative() && decimal_integral_is_odd(exponent)?;
    let absolute_base = base.abs();
    let logarithm_weight = estimate_decimal_ln_weight(&absolute_base)?;
    let probe_scale = (8 - logarithm_weight).max(0);
    let logarithm = decimal_ln(&absolute_base, probe_scale)?;
    let logarithmic_result = rounded_decimal_product(&logarithm, exponent, probe_scale)?;
    let logarithmic_value = logarithmic_result.to_f64()?;
    if logarithmic_value.abs() > f64::from(MAX_RESULT_SCALE) * 3.01 {
        if logarithmic_value > 0.0 {
            return None;
        }
        return zero.round_to_scale(MAX_DISPLAY_SCALE as i32);
    }

    let approximate_weight = logarithmic_value * std::f64::consts::LOG10_E;
    let result_scale = select_decimal_power_scale(
        approximate_weight,
        absolute_base.display_scale()?,
        exponent.display_scale()?,
    );
    let significant_digits = (result_scale + approximate_weight.trunc() as i32).max(0);
    let local_scale = (significant_digits - logarithm_weight + 8).max(0);
    let logarithm = decimal_ln(&absolute_base, local_scale)?;
    let logarithmic_result = rounded_decimal_product(&logarithm, exponent, local_scale)?;
    let mut result = decimal_exp(&logarithmic_result, result_scale)?;
    if negative_result && !result.is_zero() {
        result = result.negated();
    }
    Some(result)
}

fn decimal_ln(argument: &DecimalValue, result_scale: i32) -> Option<DecimalValue> {
    let maximum_scale = i32::try_from(MAX_FRACTIONAL_DIGITS).ok()?;
    let result_scale = result_scale.clamp(0, maximum_scale);
    let zero = DecimalValue::from_i64(0);
    if argument <= &zero {
        return None;
    }
    let one = DecimalValue::from_i64(1);
    let lower = DecimalValue::parse("0.9")?;
    let upper = DecimalValue::parse("1.1")?;
    let mut reduced = argument.clone();
    let mut square_roots = 0_i32;
    while reduced <= lower || reduced >= upper {
        let local_scale =
            (result_scale - decimal_group_weight(&reduced)? * 2 + 8).clamp(0, maximum_scale);
        reduced = reduced.sqrt_to_scale(u32::try_from(local_scale).ok()?)?;
        square_roots = square_roots.checked_add(1)?;
    }

    let local_scale = result_scale
        .checked_add(((f64::from(square_roots + 1)) * std::f64::consts::LOG10_2).trunc() as i32)?
        .checked_add(8)?
        .clamp(0, maximum_scale);
    let numerator = reduced.checked_sub(&one)?;
    let denominator = reduced.checked_add(&one)?;
    let mut result =
        numerator.checked_div_to_scale(&denominator, u32::try_from(local_scale).ok()?)?;
    let mut term_power = result.clone();
    let squared = rounded_decimal_product(&result, &result, local_scale)?;
    let mut divisor = 1_i64;
    loop {
        divisor = divisor.checked_add(2)?;
        term_power = rounded_decimal_product(&term_power, &squared, local_scale)?;
        let term = term_power.checked_div_to_scale(
            &DecimalValue::from_i64(divisor),
            u32::try_from(local_scale).ok()?,
        )?;
        if term.is_zero() {
            break;
        }
        result = result.checked_add(&term)?;
        if decimal_group_weight(&term)?
            < decimal_group_weight(&result)? - local_scale.saturating_mul(2).div_euclid(4)
        {
            break;
        }
    }
    let factor_exponent = u32::try_from(square_roots + 1).ok()?;
    let factor = DecimalValue::from_i128(1_i128.checked_shl(factor_exponent)?)?;
    rounded_decimal_product(&result, &factor, result_scale)
}

fn decimal_exp(argument: &DecimalValue, result_scale: i32) -> Option<DecimalValue> {
    let value = argument.to_f64()?;
    if value.abs() >= f64::from(MAX_RESULT_SCALE) * 3.0 {
        if value > 0.0 {
            return None;
        }
        return DecimalValue::from_i64(0).round_to_scale(result_scale);
    }
    let decimal_weight = (value * std::f64::consts::LOG10_E).trunc() as i32;
    let mut reduced = argument.clone();
    let mut divisions = 0_u32;
    let mut reduced_value = value;
    while reduced_value.abs() > 0.01 {
        divisions = divisions.checked_add(1)?;
        reduced_value /= 2.0;
    }
    if divisions > 0 {
        let divisor = DecimalValue::from_i128(1_i128.checked_shl(divisions)?)?;
        let scale = reduced
            .display_scale()?
            .checked_add(divisions)?
            .min(MAX_FRACTIONAL_DIGITS);
        reduced = reduced.checked_div_to_scale(&divisor, scale)?;
    }

    let significant_digits = (1
        + decimal_weight
        + result_scale
        + (f64::from(divisions) * std::f64::consts::LOG10_2).trunc() as i32)
        .max(0)
        + 8;
    let mut local_scale = (significant_digits - 1).max(0);
    let mut result = DecimalValue::from_i64(1).checked_add(&reduced)?;
    let mut element = rounded_decimal_product(&reduced, &reduced, local_scale)?;
    let mut divisor = 2_i64;
    element = element.checked_div_to_scale(
        &DecimalValue::from_i64(divisor),
        u32::try_from(local_scale).ok()?,
    )?;
    while !element.is_zero() {
        result = result.checked_add(&element)?;
        element = rounded_decimal_product(&element, &reduced, local_scale)?;
        divisor = divisor.checked_add(1)?;
        element = element.checked_div_to_scale(
            &DecimalValue::from_i64(divisor),
            u32::try_from(local_scale).ok()?,
        )?;
    }
    for _ in 0..divisions {
        local_scale = (significant_digits - decimal_group_weight(&result)? * 8).max(0);
        result = rounded_decimal_product(&result, &result, local_scale)?;
    }
    result.round_to_scale(result_scale)
}

pub(super) fn rounded_decimal_product(
    left: &DecimalValue,
    right: &DecimalValue,
    result_scale: i32,
) -> Option<DecimalValue> {
    left.checked_mul(right)?
        .round_to_scale(result_scale.clamp(0, i32::try_from(MAX_FRACTIONAL_DIGITS).ok()?))
}

pub(super) fn select_decimal_power_scale(
    approximate_weight: f64,
    base_scale: u32,
    exponent_scale: u32,
) -> i32 {
    let selected = 16_i32
        .saturating_sub(approximate_weight.trunc() as i32)
        .max(i32::try_from(base_scale).unwrap_or(i32::MAX))
        .max(i32::try_from(exponent_scale).unwrap_or(i32::MAX))
        .max(0);
    selected.min(MAX_DISPLAY_SCALE as i32)
}

fn estimate_decimal_ln_weight(value: &DecimalValue) -> Option<i32> {
    let lower = DecimalValue::parse("0.9")?;
    let upper = DecimalValue::parse("1.1")?;
    if value >= &lower && value <= &upper {
        let distance = value.checked_sub(&DecimalValue::from_i64(1))?.abs();
        return Some(if distance.is_zero() {
            0
        } else {
            decimal_weight(&distance)?
        });
    }
    let logarithm = value.approximate_log10_abs()? * std::f64::consts::LN_10;
    Some(logarithm.abs().log10().trunc() as i32)
}

fn decimal_weight(value: &DecimalValue) -> Option<i32> {
    let DecimalRepr::Finite { coefficient, scale } = value.repr() else {
        return None;
    };
    if coefficient.is_zero() {
        return Some(0);
    }
    i32::try_from(decimal_digit_count(coefficient))
        .ok()?
        .checked_sub(i32::try_from(*scale).ok()?)?
        .checked_sub(1)
}

fn decimal_group_weight(value: &DecimalValue) -> Option<i32> {
    numeric_group_head(value).map(|(weight, _, _)| weight)
}

fn decimal_integral_i32(value: &DecimalValue) -> Option<i32> {
    let DecimalRepr::Finite { coefficient, scale } = value.repr() else {
        return None;
    };
    let divisor = pow10(*scale);
    ((coefficient % &divisor).is_zero())
        .then(|| coefficient / divisor)?
        .to_i32()
}

fn decimal_integral_is_odd(value: &DecimalValue) -> Option<bool> {
    let DecimalRepr::Finite { coefficient, scale } = value.repr() else {
        return None;
    };
    let divisor = pow10(*scale);
    if !(coefficient % &divisor).is_zero() {
        return None;
    }
    Some(((coefficient / divisor) % 2_u8) != BigInt::zero())
}
