//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared decimal arithmetic keeps each native coefficient kernel's admitted workspace with its result.

use num_bigint::BigInt;
use num_traits::Zero;

use super::{
    coefficient, format_budgeted::coefficient_digits_with_control, DecimalRepr, DecimalValue,
    MAX_DISPLAY_SCALE, MAX_FRACTIONAL_DIGITS, MAX_INTEGER_DIGITS,
};
use crate::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

pub(super) enum Failure {
    Invalid,
    Retention(ValueRetentionError),
}

impl From<ValueRetentionError> for Failure {
    fn from(error: ValueRetentionError) -> Self {
        Self::Retention(error)
    }
}

pub(super) type Calculation<T> = Result<T, Failure>;

pub(super) fn checked<T>(result: Calculation<T>) -> Result<Option<T>, ValueRetentionError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(Failure::Invalid) => Ok(None),
        Err(Failure::Retention(error)) => Err(error),
    }
}

impl DecimalValue {
    pub fn checked_add_with_control(
        &self,
        rhs: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        checked(self.add_controlled(rhs, control))
    }

    fn add_controlled(
        &self,
        rhs: &Self,
        control: &ProductionControl<'_>,
    ) -> Calculation<Produced<Self>> {
        use DecimalRepr::{Finite, NaN, NegativeInfinity, PositiveInfinity};
        control.check()?;
        match (self.repr(), rhs.repr()) {
            (NaN, _)
            | (_, NaN)
            | (PositiveInfinity, NegativeInfinity)
            | (NegativeInfinity, PositiveInfinity) => special(NaN, control),
            (PositiveInfinity, _) | (_, PositiveInfinity) => special(PositiveInfinity, control),
            (NegativeInfinity, _) | (_, NegativeInfinity) => special(NegativeInfinity, control),
            (
                Finite {
                    coefficient: left,
                    scale: left_scale,
                },
                Finite {
                    coefficient: right,
                    scale: right_scale,
                },
            ) => {
                let scale = (*left_scale).max(*right_scale);
                let left = aligned(left, *left_scale, scale, control)?;
                let right = aligned(right, *right_scale, scale, control)?;
                finite(coefficient::add(&left, &right, control)?, scale, control)
            }
        }
    }

    pub fn checked_sub_with_control(
        &self,
        rhs: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        let rhs = rhs.negated_with_control(control)?;
        self.checked_add_with_control(&rhs, control)
    }

    pub fn checked_mul_with_control(
        &self,
        rhs: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        checked(self.multiply_controlled(rhs, control))
    }

    fn multiply_controlled(
        &self,
        rhs: &Self,
        control: &ProductionControl<'_>,
    ) -> Calculation<Produced<Self>> {
        control.check()?;
        if self.is_nan() || rhs.is_nan() {
            return special(DecimalRepr::NaN, control);
        }
        if self.is_infinite() || rhs.is_infinite() {
            if self.is_zero() || rhs.is_zero() {
                return special(DecimalRepr::NaN, control);
            }
            return infinity(self.sign() * rhs.sign(), control);
        }
        let (
            DecimalRepr::Finite {
                coefficient: left,
                scale: left_scale,
            },
            DecimalRepr::Finite {
                coefficient: right,
                scale: right_scale,
            },
        ) = (self.repr(), rhs.repr())
        else {
            unreachable!("special numeric handled above")
        };
        let coefficient = coefficient::multiply(left, right, control)?;
        let scale = left_scale
            .checked_add(*right_scale)
            .ok_or(Failure::Invalid)?;
        if scale <= MAX_FRACTIONAL_DIGITS {
            return finite(coefficient, scale, control);
        }
        let divisor = coefficient::power_of_ten(scale - MAX_FRACTIONAL_DIGITS, control)?;
        let coefficient = divide_rounded(&coefficient, &divisor, control)?;
        finite(coefficient, MAX_FRACTIONAL_DIGITS, control)
    }

    pub fn checked_div_to_scale_with_control(
        &self,
        rhs: &Self,
        result_scale: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        checked(self.divide_controlled(rhs, Some(result_scale), control))
    }

    pub fn checked_div_postgres_with_control(
        &self,
        rhs: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        checked(self.divide_controlled(rhs, None, control))
    }

    fn divide_controlled(
        &self,
        rhs: &Self,
        result_scale: Option<u32>,
        control: &ProductionControl<'_>,
    ) -> Calculation<Produced<Self>> {
        control.check()?;
        if result_scale.is_some_and(|scale| scale > MAX_FRACTIONAL_DIGITS) || rhs.is_zero() {
            return Err(Failure::Invalid);
        }
        if self.is_nan() || rhs.is_nan() || (self.is_infinite() && rhs.is_infinite()) {
            return special(DecimalRepr::NaN, control);
        }
        if self.is_infinite() {
            return infinity(self.sign() * rhs.sign(), control);
        }
        if rhs.is_infinite() {
            return Ok(Self::from_i64_with_control(0, control)?);
        }
        let (
            DecimalRepr::Finite {
                coefficient: dividend,
                scale: dividend_scale,
            },
            DecimalRepr::Finite {
                coefficient: divisor,
                scale: divisor_scale,
            },
        ) = (self.repr(), rhs.repr())
        else {
            unreachable!("special numeric handled above")
        };
        let result_scale = match result_scale {
            Some(scale) => scale,
            None => postgres_div_scale(self, rhs, control)?.min(MAX_FRACTIONAL_DIGITS),
        };
        let shift =
            i64::from(*divisor_scale) + i64::from(result_scale) - i64::from(*dividend_scale);
        let (numerator, denominator) = if shift >= 0 {
            let power = coefficient::power_of_ten(
                u32::try_from(shift).map_err(|_| Failure::Invalid)?,
                control,
            )?;
            (
                coefficient::multiply(dividend, &power, control)?,
                coefficient::clone(divisor, control)?,
            )
        } else {
            let power = coefficient::power_of_ten(
                u32::try_from(shift.checked_neg().ok_or(Failure::Invalid)?)
                    .map_err(|_| Failure::Invalid)?,
                control,
            )?;
            (
                coefficient::clone(dividend, control)?,
                coefficient::multiply(divisor, &power, control)?,
            )
        };
        finite(
            divide_rounded(&numerator, &denominator, control)?,
            result_scale,
            control,
        )
    }

    pub fn checked_rem_with_control(
        &self,
        rhs: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        checked(self.remainder_controlled(rhs, control))
    }

    fn remainder_controlled(
        &self,
        rhs: &Self,
        control: &ProductionControl<'_>,
    ) -> Calculation<Produced<Self>> {
        control.check()?;
        if rhs.is_zero() {
            return Err(Failure::Invalid);
        }
        if self.is_nan() || rhs.is_nan() || self.is_infinite() {
            return special(DecimalRepr::NaN, control);
        }
        if rhs.is_infinite() {
            return Ok(self.clone_with_control(control)?);
        }
        let (
            DecimalRepr::Finite {
                coefficient: left,
                scale: left_scale,
            },
            DecimalRepr::Finite {
                coefficient: right,
                scale: right_scale,
            },
        ) = (self.repr(), rhs.repr())
        else {
            unreachable!("special numeric handled above")
        };
        let scale = (*left_scale).max(*right_scale);
        let left = aligned(left, *left_scale, scale, control)?;
        let right = aligned(right, *right_scale, scale, control)?;
        finite(
            coefficient::remainder(&left, &right, control)?,
            scale,
            control,
        )
    }
}

pub(super) fn finite(
    value: Produced<BigInt>,
    scale: u32,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<DecimalValue>> {
    if scale > MAX_FRACTIONAL_DIGITS {
        return Err(Failure::Invalid);
    }
    let digits = coefficient_digits_with_control(&value, control)?;
    if digits.len().saturating_sub(scale as usize) > MAX_INTEGER_DIGITS {
        return Err(Failure::Invalid);
    }
    drop(digits);
    let header = control.reserve(size_of::<DecimalRepr>())?;
    let (coefficient, memory) = value.into_parts();
    Ok(control.finish(
        DecimalValue::with_repr(DecimalRepr::Finite { coefficient, scale }),
        control.combine(memory, header),
    )?)
}

pub(super) fn special(
    repr: DecimalRepr,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<DecimalValue>> {
    debug_assert!(!matches!(repr, DecimalRepr::Finite { .. }));
    let memory = control.reserve(size_of::<DecimalRepr>())?;
    Ok(control.finish(DecimalValue::with_repr(repr), memory)?)
}

pub(super) fn infinity(
    sign: i8,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<DecimalValue>> {
    special(
        if sign < 0 {
            DecimalRepr::NegativeInfinity
        } else {
            DecimalRepr::PositiveInfinity
        },
        control,
    )
}

fn aligned(
    value: &BigInt,
    source_scale: u32,
    target_scale: u32,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<BigInt>> {
    let power = coefficient::power_of_ten(target_scale - source_scale, control)?;
    Ok(coefficient::multiply(value, &power, control)?)
}

pub(super) fn divide_rounded(
    numerator: &BigInt,
    denominator: &BigInt,
    control: &ProductionControl<'_>,
) -> Calculation<Produced<BigInt>> {
    if denominator.is_zero() {
        return Err(Failure::Invalid);
    }
    let quotient = coefficient::divide(numerator, denominator, control)?;
    let remainder = coefficient::remainder(numerator, denominator, control)?;
    if remainder.is_zero() {
        return Ok(quotient);
    }
    let remainder_abs = coefficient::absolute(&remainder, control)?;
    let two = coefficient::from_i64(2, control)?;
    let doubled = coefficient::multiply(&remainder_abs, &two, control)?;
    let denominator_abs = coefficient::absolute(denominator, control)?;
    if *doubled < *denominator_abs {
        return Ok(quotient);
    }
    let sign = coefficient::from_i64(
        if numerator.sign() == denominator.sign() {
            1
        } else {
            -1
        },
        control,
    )?;
    Ok(coefficient::add(&quotient, &sign, control)?)
}

fn postgres_div_scale(
    dividend: &DecimalValue,
    divisor: &DecimalValue,
    control: &ProductionControl<'_>,
) -> Calculation<u32> {
    const MIN_SIGNIFICANT_DIGITS: i32 = 16;
    const DECIMAL_DIGITS_PER_GROUP: i32 = 4;
    let Some((dividend_weight, dividend_first_digit, dividend_scale)) =
        numeric_group_head(dividend, control)?
    else {
        return Ok(0);
    };
    let Some((divisor_weight, divisor_first_digit, divisor_scale)) =
        numeric_group_head(divisor, control)?
    else {
        return Ok(0);
    };
    let mut quotient_weight = dividend_weight - divisor_weight;
    if dividend_first_digit <= divisor_first_digit {
        quotient_weight -= 1;
    }
    let selected = MIN_SIGNIFICANT_DIGITS - quotient_weight * DECIMAL_DIGITS_PER_GROUP;
    Ok(selected
        .max(i32::try_from(dividend_scale).unwrap_or(i32::MAX))
        .max(i32::try_from(divisor_scale).unwrap_or(i32::MAX))
        .max(0)
        .min(MAX_DISPLAY_SCALE as i32) as u32)
}

pub(super) fn numeric_group_head(
    value: &DecimalValue,
    control: &ProductionControl<'_>,
) -> Result<Option<(i32, u32, u32)>, ValueRetentionError> {
    control.check()?;
    let DecimalRepr::Finite { coefficient, scale } = value.repr() else {
        return Ok(None);
    };
    if coefficient.is_zero() {
        return Ok(Some((0, 0, *scale)));
    }
    let digits = coefficient_digits_with_control(coefficient, control)?;
    let decimal_weight = i32::try_from(digits.len())
        .unwrap_or(i32::MAX)
        .saturating_sub(i32::try_from(*scale).unwrap_or(i32::MAX))
        .saturating_sub(1);
    let group_weight = decimal_weight.div_euclid(4);
    let leading_width = usize::try_from(decimal_weight - group_weight * 4 + 1).unwrap_or(4);
    let mut first_digit = digits
        .iter()
        .rev()
        .take(leading_width)
        .fold(0_u32, |number, digit| number * 10 + u32::from(*digit));
    for _ in digits.len().min(leading_width)..leading_width {
        first_digit *= 10;
    }
    Ok(Some((group_weight, first_digit, *scale)))
}

#[cfg(test)]
mod tests;
