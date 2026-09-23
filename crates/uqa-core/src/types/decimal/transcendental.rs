//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decimal square root, logarithm, exponential, and power operations.

use num_bigint::BigInt;
use num_traits::{Signed, Zero};
use std::cmp::Ordering;

use super::arithmetic_control::{
    checked, finite, numeric_group_head, special, Calculation, Failure,
};
use super::{
    coefficient, format_budgeted::coefficient_digits_with_control, power, DecimalRepr,
    DecimalValue, MAX_DISPLAY_SCALE, MAX_FRACTIONAL_DIGITS, MAX_RESULT_SCALE,
};
use crate::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

impl DecimalValue {
    /// Square root rounded to an explicit display scale. The calculation is performed entirely with integers, so it does not lose numeric digits through a binary floating-point conversion.
    pub fn sqrt_to_scale(&self, result_scale: u32) -> Option<Self> {
        self.sqrt_to_scale_with_control(result_scale, &ProductionControl::uncontrolled())
            .expect("ordinary decimal square root")
            .map(|value| value.into_uncontrolled().expect("ordinary decimal owner"))
    }

    pub fn sqrt_to_scale_with_control(
        &self,
        result_scale: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        checked(square_root(self, result_scale, control))
    }

    /// Raise one `NUMERIC` value to another using `PostgreSQL`'s result-scale selection and decimal `ln`/`exp` algorithms. Domain errors are left to the SQL layer so it can attach `PostgreSQL`'s required SQLSTATE.
    pub fn checked_pow_postgres(&self, exponent: &Self) -> Option<Self> {
        self.checked_pow_postgres_with_control(exponent, &ProductionControl::uncontrolled())
            .expect("ordinary decimal power")
            .map(|value| value.into_uncontrolled().expect("ordinary decimal owner"))
    }

    pub fn checked_pow_postgres_with_control(
        &self,
        exponent: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        checked(decimal_power(self, exponent, control))
    }

    pub(super) fn approximate_log10_abs_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Calculation<f64> {
        let DecimalRepr::Finite { coefficient, scale } = self.repr() else {
            return Err(Failure::Invalid);
        };
        if coefficient.is_zero() {
            return Err(Failure::Invalid);
        }
        let digits = coefficient_digits_with_control(coefficient, control)?;
        let take = digits.len().min(16);
        let mut leading = [0_u8; 16];
        for (out, digit) in leading.iter_mut().zip(digits.iter().rev()).take(take) {
            *out = b'0' + digit;
        }
        let leading = std::str::from_utf8(&leading[..take])
            .expect("decimal coefficient digits")
            .parse::<f64>()
            .map_err(|_| Failure::Invalid)?;
        let mantissa = leading
            / 10_f64.powi(
                i32::try_from(take)
                    .map_err(|_| Failure::Invalid)?
                    .saturating_sub(1),
            );
        let weight = i32::try_from(digits.len())
            .map_err(|_| Failure::Invalid)?
            .checked_sub(i32::try_from(*scale).map_err(|_| Failure::Invalid)?)
            .and_then(|weight| weight.checked_sub(1))
            .ok_or(Failure::Invalid)?;
        Ok(f64::from(weight) + mantissa.log10())
    }
}

fn square_root(
    value: &DecimalValue,
    result_scale: u32,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<DecimalValue>> {
    control.check()?;
    if result_scale > MAX_FRACTIONAL_DIGITS || value.is_negative_infinity() {
        return Err(Failure::Invalid);
    }
    if value.is_nan() {
        return special(DecimalRepr::NaN, control);
    }
    if value.is_positive_infinity() {
        return special(DecimalRepr::PositiveInfinity, control);
    }
    let DecimalRepr::Finite {
        coefficient: value,
        scale,
    } = value.repr()
    else {
        unreachable!("special numeric handled above")
    };
    if value.is_negative() {
        return Err(Failure::Invalid);
    }
    let exponent = i64::from(result_scale)
        .checked_mul(2)
        .and_then(|value| value.checked_sub(i64::from(*scale)))
        .ok_or(Failure::Invalid)?;
    let (radicand, denominator) = if exponent >= 0 {
        let power = coefficient::power_of_ten(
            u32::try_from(exponent).map_err(|_| Failure::Invalid)?,
            control,
        )?;
        (
            coefficient::multiply(value, &power, control)?,
            coefficient::from_i64(1, control)?,
        )
    } else {
        (
            coefficient::clone(value, control)?,
            coefficient::power_of_ten(
                u32::try_from(exponent.checked_neg().ok_or(Failure::Invalid)?)
                    .map_err(|_| Failure::Invalid)?,
                control,
            )?,
        )
    };
    let quotient = coefficient::divide(&radicand, &denominator, control)?;
    let mut root = integer_sqrt_floor(&quotient, control)?;
    let one = coefficient::from_i64(1, control)?;
    let two = coefficient::from_i64(2, control)?;
    let four = coefficient::from_i64(4, control)?;
    let midpoint = coefficient::add(
        &*coefficient::multiply(&root, &two, control)?,
        &one,
        control,
    )?;
    let left = coefficient::multiply(&radicand, &four, control)?;
    let right = coefficient::multiply(
        &*coefficient::multiply(&denominator, &midpoint, control)?,
        &midpoint,
        control,
    )?;
    if *left >= *right {
        root = coefficient::add(&root, &one, control)?;
    }
    finite(root, result_scale, control)
}

fn integer_sqrt_floor(
    value: &BigInt,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<BigInt>> {
    debug_assert!(!value.is_negative());
    let one = coefficient::from_i64(1, control)?;
    if value <= &*one {
        return Ok(coefficient::clone(value, control)?);
    }
    let digits = coefficient_digits_with_control(value, control)?.len();
    let initial_power = u32::try_from(digits.div_ceil(2)).unwrap_or(u32::MAX);
    let mut estimate = coefficient::power_of_ten(initial_power, control)?;
    let two = coefficient::from_i64(2, control)?;
    loop {
        control.check()?;
        let quotient = coefficient::divide(value, &estimate, control)?;
        let sum = coefficient::add(&estimate, &quotient, control)?;
        let next = coefficient::divide(&sum, &two, control)?;
        if *next >= *estimate {
            return Ok(estimate);
        }
        estimate = next;
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the existing numeric power algorithm keeps domain and scale precedence in one dispatch"
)]
fn decimal_power(
    base: &DecimalValue,
    exponent: &DecimalValue,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<DecimalValue>> {
    let zero = DecimalValue::from_i64_with_control(0, control)?;
    let one = DecimalValue::from_i64_with_control(1, control)?;
    if base.is_nan() {
        return if exponent.is_zero() {
            Ok(one)
        } else {
            special(DecimalRepr::NaN, control)
        };
    }
    if exponent.is_nan() {
        return if base.cmp_with_control(&one, control)? == Ordering::Equal {
            Ok(one)
        } else {
            special(DecimalRepr::NaN, control)
        };
    }
    if base.is_zero() && exponent.is_negative() {
        return Err(Failure::Invalid);
    }
    if base.is_negative() && !exponent.is_integral_with_control(control)? {
        return Err(Failure::Invalid);
    }
    if exponent.is_infinite() {
        let absolute = base.abs_with_control(control)?;
        let order = absolute.cmp_with_control(&one, control)?;
        if order == Ordering::Equal {
            return Ok(one);
        }
        let grows = (order == Ordering::Greater) == exponent.is_positive_infinity();
        return if grows {
            special(DecimalRepr::PositiveInfinity, control)
        } else {
            Ok(zero)
        };
    }
    if base.is_positive_infinity() {
        return if exponent.is_zero() {
            Ok(one)
        } else if exponent.is_negative() {
            Ok(zero)
        } else {
            special(DecimalRepr::PositiveInfinity, control)
        };
    }
    if base.is_negative_infinity() {
        if exponent.is_zero() {
            return Ok(one);
        }
        if exponent.is_negative() {
            return Ok(zero);
        }
        return special(
            if decimal_integral_is_odd(exponent, control)? {
                DecimalRepr::NegativeInfinity
            } else {
                DecimalRepr::PositiveInfinity
            },
            control,
        );
    }
    if let Some(integer) = decimal_integral_i32(exponent, control)? {
        return power::decimal_power_integer(
            base,
            integer,
            exponent.display_scale().ok_or(Failure::Invalid)?,
            control,
        );
    }
    if base.is_zero() {
        return round(&zero, 16, control);
    }
    let negative_result = base.is_negative() && decimal_integral_is_odd(exponent, control)?;
    let absolute_base = base.abs_with_control(control)?;
    let logarithm_weight = estimate_decimal_ln_weight(&absolute_base, control)?;
    let probe_scale = (8 - logarithm_weight).max(0);
    let logarithm = decimal_ln(&absolute_base, probe_scale, control)?;
    let logarithmic_result = rounded_decimal_product(&logarithm, exponent, probe_scale, control)?;
    let logarithmic_value = logarithmic_result
        .to_f64_with_control(control)?
        .ok_or(Failure::Invalid)?;
    if logarithmic_value.abs() > f64::from(MAX_RESULT_SCALE) * 3.01 {
        if logarithmic_value > 0.0 {
            return Err(Failure::Invalid);
        }
        return round(&zero, MAX_DISPLAY_SCALE as i32, control);
    }
    let approximate_weight = logarithmic_value * std::f64::consts::LOG10_E;
    let result_scale = select_decimal_power_scale(
        approximate_weight,
        absolute_base.display_scale().ok_or(Failure::Invalid)?,
        exponent.display_scale().ok_or(Failure::Invalid)?,
    );
    let significant_digits = (result_scale + approximate_weight.trunc() as i32).max(0);
    let local_scale = (significant_digits - logarithm_weight + 8).max(0);
    let logarithm = decimal_ln(&absolute_base, local_scale, control)?;
    let logarithmic_result = rounded_decimal_product(&logarithm, exponent, local_scale, control)?;
    let mut result = decimal_exp(&logarithmic_result, result_scale, control)?;
    if negative_result && !result.is_zero() {
        result = result.negated_with_control(control)?;
    }
    Ok(result)
}

fn decimal_ln(
    argument: &DecimalValue,
    result_scale: i32,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<DecimalValue>> {
    let maximum_scale = i32::try_from(MAX_FRACTIONAL_DIGITS).map_err(|_| Failure::Invalid)?;
    let result_scale = result_scale.clamp(0, maximum_scale);
    let zero = DecimalValue::from_i64_with_control(0, control)?;
    if argument.cmp_with_control(&zero, control)? != Ordering::Greater {
        return Err(Failure::Invalid);
    }
    let one = DecimalValue::from_i64_with_control(1, control)?;
    let lower = DecimalValue::parse_with_control("0.9", control)?.ok_or(Failure::Invalid)?;
    let upper = DecimalValue::parse_with_control("1.1", control)?.ok_or(Failure::Invalid)?;
    let mut reduced = argument.clone_with_control(control)?;
    let mut square_roots = 0_i32;
    while reduced.cmp_with_control(&lower, control)? != Ordering::Greater
        || reduced.cmp_with_control(&upper, control)? != Ordering::Less
    {
        control.check()?;
        let local_scale = (result_scale - decimal_group_weight(&reduced, control)? * 2 + 8)
            .clamp(0, maximum_scale);
        reduced = square_root(
            &reduced,
            u32::try_from(local_scale).map_err(|_| Failure::Invalid)?,
            control,
        )?;
        square_roots = square_roots.checked_add(1).ok_or(Failure::Invalid)?;
    }
    let local_scale = result_scale
        .checked_add(((f64::from(square_roots + 1)) * std::f64::consts::LOG10_2).trunc() as i32)
        .and_then(|scale| scale.checked_add(8))
        .ok_or(Failure::Invalid)?
        .clamp(0, maximum_scale);
    let numerator = reduced
        .checked_sub_with_control(&one, control)?
        .ok_or(Failure::Invalid)?;
    let denominator = reduced
        .checked_add_with_control(&one, control)?
        .ok_or(Failure::Invalid)?;
    let mut result = numerator
        .checked_div_to_scale_with_control(
            &denominator,
            u32::try_from(local_scale).map_err(|_| Failure::Invalid)?,
            control,
        )?
        .ok_or(Failure::Invalid)?;
    let mut term_power = result.clone_with_control(control)?;
    let squared = rounded_decimal_product(&result, &result, local_scale, control)?;
    let mut divisor = 1_i64;
    loop {
        control.check()?;
        divisor = divisor.checked_add(2).ok_or(Failure::Invalid)?;
        term_power = rounded_decimal_product(&term_power, &squared, local_scale, control)?;
        let divisor_value = DecimalValue::from_i64_with_control(divisor, control)?;
        let term = term_power
            .checked_div_to_scale_with_control(
                &divisor_value,
                u32::try_from(local_scale).map_err(|_| Failure::Invalid)?,
                control,
            )?
            .ok_or(Failure::Invalid)?;
        if term.is_zero() {
            break;
        }
        result = result
            .checked_add_with_control(&term, control)?
            .ok_or(Failure::Invalid)?;
        if decimal_group_weight(&term, control)?
            < decimal_group_weight(&result, control)? - local_scale.saturating_mul(2).div_euclid(4)
        {
            break;
        }
    }
    let factor_exponent = u32::try_from(square_roots + 1).map_err(|_| Failure::Invalid)?;
    let factor = integer128(
        1_i128
            .checked_shl(factor_exponent)
            .ok_or(Failure::Invalid)?,
        control,
    )?;
    rounded_decimal_product(&result, &factor, result_scale, control)
}

fn decimal_exp(
    argument: &DecimalValue,
    result_scale: i32,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<DecimalValue>> {
    let value = argument
        .to_f64_with_control(control)?
        .ok_or(Failure::Invalid)?;
    if value.abs() >= f64::from(MAX_RESULT_SCALE) * 3.0 {
        if value > 0.0 {
            return Err(Failure::Invalid);
        }
        return round(
            &*DecimalValue::from_i64_with_control(0, control)?,
            result_scale,
            control,
        );
    }
    let decimal_weight = (value * std::f64::consts::LOG10_E).trunc() as i32;
    let mut reduced = argument.clone_with_control(control)?;
    let mut divisions = 0_u32;
    let mut reduced_value = value;
    while reduced_value.abs() > 0.01 {
        control.check()?;
        divisions = divisions.checked_add(1).ok_or(Failure::Invalid)?;
        reduced_value /= 2.0;
    }
    if divisions > 0 {
        let divisor = integer128(
            1_i128.checked_shl(divisions).ok_or(Failure::Invalid)?,
            control,
        )?;
        let scale = reduced
            .display_scale()
            .ok_or(Failure::Invalid)?
            .checked_add(divisions)
            .ok_or(Failure::Invalid)?
            .min(MAX_FRACTIONAL_DIGITS);
        reduced = reduced
            .checked_div_to_scale_with_control(&divisor, scale, control)?
            .ok_or(Failure::Invalid)?;
    }
    let significant_digits = (1
        + decimal_weight
        + result_scale
        + (f64::from(divisions) * std::f64::consts::LOG10_2).trunc() as i32)
        .max(0)
        + 8;
    let mut local_scale = (significant_digits - 1).max(0);
    let one = DecimalValue::from_i64_with_control(1, control)?;
    let mut result = one
        .checked_add_with_control(&reduced, control)?
        .ok_or(Failure::Invalid)?;
    let mut element = rounded_decimal_product(&reduced, &reduced, local_scale, control)?;
    let mut divisor = 2_i64;
    element = element
        .checked_div_to_scale_with_control(
            &*DecimalValue::from_i64_with_control(divisor, control)?,
            u32::try_from(local_scale).map_err(|_| Failure::Invalid)?,
            control,
        )?
        .ok_or(Failure::Invalid)?;
    while !element.is_zero() {
        control.check()?;
        result = result
            .checked_add_with_control(&element, control)?
            .ok_or(Failure::Invalid)?;
        element = rounded_decimal_product(&element, &reduced, local_scale, control)?;
        divisor = divisor.checked_add(1).ok_or(Failure::Invalid)?;
        element = element
            .checked_div_to_scale_with_control(
                &*DecimalValue::from_i64_with_control(divisor, control)?,
                u32::try_from(local_scale).map_err(|_| Failure::Invalid)?,
                control,
            )?
            .ok_or(Failure::Invalid)?;
    }
    for _ in 0..divisions {
        control.check()?;
        local_scale = (significant_digits - decimal_group_weight(&result, control)? * 8).max(0);
        result = rounded_decimal_product(&result, &result, local_scale, control)?;
    }
    round(&result, result_scale, control)
}

fn integer128(value: i128, control: &ProductionControl<'_>) -> Calculation<Produced<DecimalValue>> {
    let text = control.format(format_args!("{value}"))?;
    DecimalValue::parse_with_control(&text, control)?.ok_or(Failure::Invalid)
}

pub(super) fn round(
    value: &DecimalValue,
    scale: i32,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<DecimalValue>> {
    value
        .round_to_scale_with_control(scale, control)?
        .ok_or(Failure::Invalid)
}

pub(super) fn rounded_decimal_product(
    left: &DecimalValue,
    right: &DecimalValue,
    result_scale: i32,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<DecimalValue>> {
    let product = left
        .checked_mul_with_control(right, control)?
        .ok_or(Failure::Invalid)?;
    round(
        &product,
        result_scale.clamp(
            0,
            i32::try_from(MAX_FRACTIONAL_DIGITS).map_err(|_| Failure::Invalid)?,
        ),
        control,
    )
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

fn estimate_decimal_ln_weight(
    value: &DecimalValue,
    control: &ProductionControl<'_>,
) -> Calculation<i32> {
    let lower = DecimalValue::parse_with_control("0.9", control)?.ok_or(Failure::Invalid)?;
    let upper = DecimalValue::parse_with_control("1.1", control)?.ok_or(Failure::Invalid)?;
    if value.cmp_with_control(&lower, control)? != Ordering::Less
        && value.cmp_with_control(&upper, control)? != Ordering::Greater
    {
        let difference = value
            .checked_sub_with_control(&*DecimalValue::from_i64_with_control(1, control)?, control)?
            .ok_or(Failure::Invalid)?;
        let distance = difference.abs_with_control(control)?;
        return if distance.is_zero() {
            Ok(0)
        } else {
            decimal_weight(&distance, control)
        };
    }
    let logarithm = value.approximate_log10_abs_with_control(control)? * std::f64::consts::LN_10;
    Ok(logarithm.abs().log10().trunc() as i32)
}

fn decimal_weight(value: &DecimalValue, control: &ProductionControl<'_>) -> Calculation<i32> {
    let DecimalRepr::Finite { coefficient, scale } = value.repr() else {
        return Err(Failure::Invalid);
    };
    if coefficient.is_zero() {
        return Ok(0);
    }
    let digits = coefficient_digits_with_control(coefficient, control)?.len();
    i32::try_from(digits)
        .map_err(|_| Failure::Invalid)?
        .checked_sub(i32::try_from(*scale).map_err(|_| Failure::Invalid)?)
        .and_then(|weight| weight.checked_sub(1))
        .ok_or(Failure::Invalid)
}

pub(super) fn decimal_group_weight(
    value: &DecimalValue,
    control: &ProductionControl<'_>,
) -> Calculation<i32> {
    numeric_group_head(value, control)?
        .map(|(weight, _, _)| weight)
        .ok_or(Failure::Invalid)
}

fn decimal_integral_i32(
    value: &DecimalValue,
    control: &ProductionControl<'_>,
) -> Calculation<Option<i32>> {
    if !value.is_integral_with_control(control)? {
        return Ok(None);
    }
    let text = value.to_sql_string_with_control(control)?;
    Ok(text
        .split_once('.')
        .map_or(&**text, |(integer, _)| integer)
        .parse()
        .ok())
}

fn decimal_integral_is_odd(
    value: &DecimalValue,
    control: &ProductionControl<'_>,
) -> Calculation<bool> {
    let DecimalRepr::Finite { coefficient, scale } = value.repr() else {
        return Err(Failure::Invalid);
    };
    let digits = coefficient_digits_with_control(coefficient, control)?;
    for digit in digits.iter().take(*scale as usize) {
        control.check()?;
        if *digit != 0 {
            return Err(Failure::Invalid);
        }
    }
    Ok(digits
        .get(*scale as usize)
        .is_some_and(|digit| digit % 2 != 0))
}
