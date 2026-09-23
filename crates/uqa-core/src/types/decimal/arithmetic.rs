//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact arithmetic, division-scale selection, and decimal quantization.

use num_bigint::BigInt;
use num_traits::{Signed, Zero};

use super::{
    align_coefficient, pow10, DecimalRepr, DecimalValue, MAX_DISPLAY_SCALE, MAX_FRACTIONAL_DIGITS,
};

impl DecimalValue {
    pub fn is_integral(&self) -> bool {
        match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => {
                *scale == 0 || (coefficient % pow10(*scale)).is_zero()
            }
            DecimalRepr::NegativeInfinity | DecimalRepr::PositiveInfinity => true,
            DecimalRepr::NaN => false,
        }
    }

    pub fn checked_add(&self, rhs: &Self) -> Option<Self> {
        use DecimalRepr::{Finite, NaN, NegativeInfinity, PositiveInfinity};
        match (self.repr(), rhs.repr()) {
            (NaN, _) | (_, NaN) => Some(Self::nan()),
            (PositiveInfinity, NegativeInfinity) | (NegativeInfinity, PositiveInfinity) => {
                Some(Self::nan())
            }
            (PositiveInfinity, _) | (_, PositiveInfinity) => Some(Self::positive_infinity()),
            (NegativeInfinity, _) | (_, NegativeInfinity) => Some(Self::negative_infinity()),
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
                let left = align_coefficient(left, *left_scale, scale);
                let right = align_coefficient(right, *right_scale, scale);
                Self::finite(left + right, scale)
            }
        }
    }

    pub fn checked_sub(&self, rhs: &Self) -> Option<Self> {
        self.checked_add(&rhs.negated())
    }

    pub fn checked_mul(&self, rhs: &Self) -> Option<Self> {
        use DecimalRepr::{Finite, NaN};
        if matches!(self.repr(), NaN) || matches!(rhs.repr(), NaN) {
            return Some(Self::nan());
        }
        if self.is_infinite() || rhs.is_infinite() {
            if self.is_zero() || rhs.is_zero() {
                return Some(Self::nan());
            }
            return Some(Self::infinity_with_sign(self.sign() * rhs.sign()));
        }
        let (
            Finite {
                coefficient: left,
                scale: left_scale,
            },
            Finite {
                coefficient: right,
                scale: right_scale,
            },
        ) = (self.repr(), rhs.repr())
        else {
            unreachable!("special numeric handled above")
        };
        let coefficient = left * right;
        let scale = left_scale.checked_add(*right_scale)?;
        if scale <= MAX_FRACTIONAL_DIGITS {
            return Self::finite(coefficient, scale);
        }
        let divisor = pow10(scale - MAX_FRACTIONAL_DIGITS);
        let coefficient = divide_round_away_from_zero(&coefficient, &divisor)?;
        Self::finite(coefficient, MAX_FRACTIONAL_DIGITS)
    }

    pub fn checked_div(&self, rhs: &Self) -> Option<Self> {
        self.checked_div_postgres(rhs)
    }

    /// Divide at an explicit display scale with `PostgreSQL` numeric midpoint rounding. This is used by algorithms whose result scale is selected by the caller rather than by `select_div_scale()`.
    pub fn checked_div_to_scale(&self, rhs: &Self, result_scale: u32) -> Option<Self> {
        if result_scale > MAX_FRACTIONAL_DIGITS || rhs.is_zero() {
            return None;
        }
        if self.is_nan() || rhs.is_nan() {
            return Some(Self::nan());
        }
        if self.is_infinite() && rhs.is_infinite() {
            return Some(Self::nan());
        }
        if self.is_infinite() {
            return Some(Self::infinity_with_sign(self.sign() * rhs.sign()));
        }
        if rhs.is_infinite() {
            return Some(Self::from_i64(0));
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
        let shift =
            i64::from(*divisor_scale) + i64::from(result_scale) - i64::from(*dividend_scale);
        let (numerator, denominator) = if shift >= 0 {
            (
                dividend * pow10(u32::try_from(shift).ok()?),
                divisor.clone(),
            )
        } else {
            (
                dividend.clone(),
                divisor * pow10(u32::try_from(shift.checked_neg()?).ok()?),
            )
        };
        let coefficient = divide_round_away_from_zero(&numerator, &denominator)?;
        Self::finite(coefficient, result_scale)
    }

    /// Divide using `PostgreSQL`'s `select_div_scale()` rule: at least sixteen significant digits, never less scale than either finite operand, and midpoint rounding away from zero.
    pub fn checked_div_postgres(&self, rhs: &Self) -> Option<Self> {
        if rhs.is_zero() {
            return None;
        }
        if self.is_nan() || rhs.is_nan() {
            return Some(Self::nan());
        }
        if self.is_infinite() && rhs.is_infinite() {
            return Some(Self::nan());
        }
        if self.is_infinite() {
            return Some(Self::infinity_with_sign(self.sign() * rhs.sign()));
        }
        if rhs.is_infinite() {
            return Some(Self::from_i64(0));
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
        let result_scale = postgres_div_scale(self, rhs).min(MAX_FRACTIONAL_DIGITS);
        let shift =
            i64::from(*divisor_scale) + i64::from(result_scale) - i64::from(*dividend_scale);
        let (numerator, denominator) = if shift >= 0 {
            (
                dividend * pow10(u32::try_from(shift).ok()?),
                divisor.clone(),
            )
        } else {
            (
                dividend.clone(),
                divisor * pow10(u32::try_from(shift.checked_neg()?).ok()?),
            )
        };
        let coefficient = divide_round_away_from_zero(&numerator, &denominator)?;
        Self::finite(coefficient, result_scale)
    }

    pub fn checked_rem(&self, rhs: &Self) -> Option<Self> {
        if rhs.is_zero() {
            return None;
        }
        if self.is_nan() || rhs.is_nan() || self.is_infinite() {
            return Some(Self::nan());
        }
        if rhs.is_infinite() {
            return Some(self.clone());
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
        let left = align_coefficient(left, *left_scale, scale);
        let right = align_coefficient(right, *right_scale, scale);
        Self::finite(left % right, scale)
    }

    pub fn abs(&self) -> Self {
        self.abs_with_control(&crate::memory::ProductionControl::uncontrolled())
            .expect("ordinary decimal absolute value")
            .into_uncontrolled()
            .expect("ordinary decimal owner")
    }

    pub fn ceil(&self) -> Self {
        self.integral_round(IntegralRounding::Ceil)
    }

    pub fn floor(&self) -> Self {
        self.integral_round(IntegralRounding::Floor)
    }

    pub fn trunc(&self) -> Self {
        self.integral_round(IntegralRounding::Trunc)
    }

    pub fn round_dp(&self, scale: u32) -> Self {
        i32::try_from(scale)
            .ok()
            .and_then(|scale| self.round_to_scale(scale))
            .unwrap_or_else(|| self.clone())
    }

    pub fn round_to_scale(&self, scale: i32) -> Option<Self> {
        self.quantize(scale, true)
    }

    pub fn trunc_to_scale(&self, scale: i32) -> Option<Self> {
        self.quantize(scale, false)
    }

    pub fn fits_precision(&self, precision: u32, scale: i32) -> bool {
        self.fits_precision_with_control(
            precision,
            scale,
            &crate::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary numeric precision")
    }

    fn integral_round(&self, rounding: IntegralRounding) -> Self {
        self.integral_round_with_control(
            rounding,
            &crate::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary decimal integral rounding")
        .into_uncontrolled()
        .expect("ordinary decimal owner")
    }

    fn quantize(&self, target_scale: i32, round: bool) -> Option<Self> {
        self.quantize_with_control(
            target_scale,
            round,
            &crate::memory::ProductionControl::uncontrolled(),
        )
        .ok()??
        .into_uncontrolled()
        .ok()
    }
}

#[derive(Clone, Copy)]
pub(super) enum IntegralRounding {
    Ceil,
    Floor,
    Trunc,
}

pub(super) fn divide_round_away_from_zero(
    numerator: &BigInt,
    denominator: &BigInt,
) -> Option<BigInt> {
    if denominator.is_zero() {
        return None;
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder.is_zero() || remainder.abs() * 2_u8 < denominator.abs() {
        return Some(quotient);
    }
    let sign = if numerator.sign() == denominator.sign() {
        1
    } else {
        -1
    };
    Some(quotient + sign)
}

fn postgres_div_scale(dividend: &DecimalValue, divisor: &DecimalValue) -> u32 {
    const MIN_SIGNIFICANT_DIGITS: i32 = 16;
    const DECIMAL_DIGITS_PER_GROUP: i32 = 4;
    let Some((dividend_weight, dividend_first_digit, dividend_scale)) =
        numeric_group_head(dividend)
    else {
        return 0;
    };
    let Some((divisor_weight, divisor_first_digit, divisor_scale)) = numeric_group_head(divisor)
    else {
        return 0;
    };
    let mut quotient_weight = dividend_weight - divisor_weight;
    if dividend_first_digit <= divisor_first_digit {
        quotient_weight -= 1;
    }
    let selected = MIN_SIGNIFICANT_DIGITS - quotient_weight * DECIMAL_DIGITS_PER_GROUP;
    selected
        .max(i32::try_from(dividend_scale).unwrap_or(i32::MAX))
        .max(i32::try_from(divisor_scale).unwrap_or(i32::MAX))
        .max(0)
        .min(MAX_DISPLAY_SCALE as i32) as u32
}

/// Return `PostgreSQL` `NumericVar`'s normalized base-10000 weight, first non-zero digit, and display scale.
pub(super) fn numeric_group_head(value: &DecimalValue) -> Option<(i32, u32, u32)> {
    let DecimalRepr::Finite { coefficient, scale } = value.repr() else {
        return None;
    };
    if coefficient.is_zero() {
        return Some((0, 0, *scale));
    }
    let digits = coefficient.abs().to_str_radix(10);
    let decimal_weight = i32::try_from(digits.len())
        .unwrap_or(i32::MAX)
        .saturating_sub(i32::try_from(*scale).unwrap_or(i32::MAX))
        .saturating_sub(1);
    let group_weight = decimal_weight.div_euclid(4);
    let leading_width = usize::try_from(decimal_weight - group_weight * 4 + 1).unwrap_or(4);
    let mut first_digit = digits
        .bytes()
        .take(leading_width)
        .fold(0_u32, |number, digit| {
            number * 10 + u32::from(digit.saturating_sub(b'0'))
        });
    for _ in digits.len().min(leading_width)..leading_width {
        first_digit *= 10;
    }
    Some((group_weight, first_digit, *scale))
}
