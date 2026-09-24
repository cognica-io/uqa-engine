//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cast conversions transfer admitted decimal representations and quantize their decimal digits.

use num_bigint::BigInt;
use num_traits::Signed;

use super::arithmetic::IntegralRounding;

use super::{DecimalRepr, DecimalValue, MAX_FRACTIONAL_DIGITS};
use crate::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

#[derive(Clone, Copy)]
enum Rounding {
    Nearest,
    Truncate,
    Up,
    Down,
}

impl DecimalValue {
    pub fn is_integral_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<bool, ValueRetentionError> {
        control.check()?;
        match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => {
                if *scale == 0 || self.is_zero() {
                    return Ok(true);
                }
                // Divisibility by 10^scale is exactly the condition that every discarded least-significant decimal digit is zero.
                let digits =
                    super::format_budgeted::coefficient_digits_with_control(coefficient, control)?;
                for digit in digits.iter().take(*scale as usize) {
                    control.check()?;
                    if *digit != 0 {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            DecimalRepr::NegativeInfinity | DecimalRepr::PositiveInfinity => Ok(true),
            DecimalRepr::NaN => Ok(false),
        }
    }

    pub fn abs_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        // abs clones one coefficient without changing its magnitude; the existing coefficient lease covers the copy and its boxed representation.
        let memory = control.reserve(self.retained_bytes())?;
        let repr = match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => DecimalRepr::Finite {
                coefficient: coefficient.abs(),
                scale: *scale,
            },
            DecimalRepr::NegativeInfinity | DecimalRepr::PositiveInfinity => {
                DecimalRepr::PositiveInfinity
            }
            DecimalRepr::NaN => DecimalRepr::NaN,
        };
        control.finish(Self::with_repr(repr), memory)
    }

    pub fn ceil_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        self.integral_round_with_control(IntegralRounding::Ceil, control)
    }

    pub fn floor_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        self.integral_round_with_control(IntegralRounding::Floor, control)
    }

    pub(super) fn integral_round_with_control(
        &self,
        rounding: IntegralRounding,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let mode = match rounding {
            IntegralRounding::Ceil => Rounding::Up,
            IntegralRounding::Floor => Rounding::Down,
            IntegralRounding::Trunc => Rounding::Truncate,
        };
        Ok(self
            .quantize_with_rounding(0, mode, control)?
            .expect("zero decimal scale"))
    }

    pub fn negated_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let memory = control.reserve(self.retained_bytes())?;
        control.finish(self.negated(), memory)
    }

    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let memory = control.reserve(self.retained_bytes())?;
        control.finish(self.clone(), memory)
    }

    pub fn from_i64_with_control(
        value: i64,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        // Primitive construction has at most two native coefficient limbs. Preserve the decimal owner's normalized four-times-limb plus three-limb slack contract before constructing either the coefficient or boxed representation.
        let bits = (u64::BITS - value.unsigned_abs().leading_zeros()) as usize;
        let bytes = size_of::<DecimalRepr>()
            + (bits.div_ceil(usize::BITS as usize).max(1) * 4 + 3) * size_of::<usize>();
        let memory = control.reserve(bytes)?;
        control.finish(
            Self::with_repr(DecimalRepr::Finite {
                coefficient: BigInt::from(value),
                scale: 0,
            }),
            memory,
        )
    }

    pub fn from_f64_lossy_with_control(
        value: f64,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        let text = control.format(format_args!("{value}"))?;
        let parsed = Self::parse_with_control(&text, control)?;
        Ok(parsed.filter(|parsed| value == 0.0 || !parsed.is_zero()))
    }

    pub fn to_i64_trunc_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<i64>, ValueRetentionError> {
        let text = self.to_sql_string_with_control(control)?;
        let integer = text.split_once('.').map_or(&**text, |(integer, _)| integer);
        let value = integer.parse().ok();
        control.check()?;
        Ok(value)
    }

    pub fn to_f64_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<f64>, ValueRetentionError> {
        control.check()?;
        match self.repr() {
            DecimalRepr::NegativeInfinity => Ok(Some(f64::NEG_INFINITY)),
            DecimalRepr::PositiveInfinity => Ok(Some(f64::INFINITY)),
            DecimalRepr::NaN => Ok(Some(f64::NAN)),
            DecimalRepr::Finite { .. } => {
                let text = self.to_sql_string_with_control(control)?;
                let value = text.parse::<f64>().ok().filter(|value| value.is_finite());
                control.check()?;
                Ok(value)
            }
        }
    }

    pub fn round_to_scale_with_control(
        &self,
        scale: i32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        self.quantize_with_control(scale, true, control)
    }

    /// Truncate decimal digits using the same admitted quantizer as scale rounding.
    pub fn trunc_to_scale_with_control(
        &self,
        scale: i32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        self.quantize_with_control(scale, false, control)
    }

    pub fn fits_precision_with_control(
        &self,
        precision: u32,
        scale: i32,
        control: &ProductionControl<'_>,
    ) -> Result<bool, ValueRetentionError> {
        control.check()?;
        let DecimalRepr::Finite {
            scale: value_scale, ..
        } = self.repr()
        else {
            return Ok(self.is_nan());
        };
        if scale == i32::MIN {
            return Ok(false);
        }
        let text = self.to_sql_string_with_control(control)?;
        let mut significant = 0_i64;
        for byte in text.bytes() {
            control.check()?;
            if byte.is_ascii_digit() && (significant != 0 || byte != b'0') {
                significant += 1;
            }
        }
        let digits = if significant == 0 {
            1
        } else {
            (significant + i64::from(scale) - i64::from(*value_scale)).max(1)
        };
        Ok(digits <= i64::from(precision))
    }

    pub(super) fn quantize_with_control(
        &self,
        target_scale: i32,
        round: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        self.quantize_with_rounding(
            target_scale,
            if round {
                Rounding::Nearest
            } else {
                Rounding::Truncate
            },
            control,
        )
    }

    fn quantize_with_rounding(
        &self,
        target_scale: i32,
        rounding: Rounding,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        control.check()?;
        if !matches!(self.repr(), DecimalRepr::Finite { .. }) {
            return self.clone_with_control(control).map(Some);
        }
        if target_scale > MAX_FRACTIONAL_DIGITS as i32 || target_scale == i32::MIN {
            return Ok(None);
        }
        let text = self.to_sql_string_with_control(control)?;
        let unsigned = text.strip_prefix('-').unwrap_or(&text);
        let point = unsigned.find('.').unwrap_or(unsigned.len());
        let digits = unsigned.len() - usize::from(point != unsigned.len());
        let cutoff = point as i64 + i64::from(target_scale);
        let kept = usize::try_from(cutoff.max(0)).expect("bounded decimal scale");
        let digit = |index: usize| {
            unsigned
                .as_bytes()
                .get(index + usize::from(index >= point))
                .copied()
                .unwrap_or(b'0')
        };
        let round_up = match rounding {
            Rounding::Nearest => cutoff >= 0 && kept < digits && digit(kept) >= b'5',
            Rounding::Truncate => false,
            Rounding::Up | Rounding::Down => {
                let toward_infinity = match rounding {
                    Rounding::Up => !self.is_negative(),
                    Rounding::Down => self.is_negative(),
                    _ => unreachable!("directed integral rounding"),
                };
                let mut discarded_nonzero = false;
                for index in kept..digits {
                    control.check()?;
                    discarded_nonzero |= digit(index) != b'0';
                }
                toward_infinity && discarded_nonzero
            }
        };
        let mut increment = None;
        if round_up {
            for index in (0..kept).rev() {
                control.check()?;
                if digit(index) != b'9' {
                    increment = Some(index);
                    break;
                }
            }
        }
        let carry = round_up && increment.is_none();
        let mut coefficient = ProductionString::new(*control);
        if self.is_negative() {
            coefficient.push('-')?;
        }
        if carry {
            coefficient.push('1')?;
        }
        if kept == 0 && !carry {
            coefficient.push('0')?;
        }
        for index in 0..kept {
            let value = if carry || increment.is_some_and(|last| index > last) {
                b'0'
            } else if increment == Some(index) {
                digit(index) + 1
            } else {
                digit(index)
            };
            coefficient.push(char::from(value))?;
        }
        coefficient.push_str(&control.format(format_args!("e{}", -target_scale))?)?;
        Self::parse_with_control(&coefficient, control)
    }
}

#[cfg(test)]
mod tests;
