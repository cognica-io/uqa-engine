//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Arbitrary-precision base-10 values with `PostgreSQL` numeric semantics.

mod power;

use super::{Deserialize, Deserializer, Ordering, Serialize, Serializer};
use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

const MAX_INTEGER_DIGITS: usize = 131_072;
const MAX_FRACTIONAL_DIGITS: u32 = 16_383;
const MAX_DISPLAY_SCALE: u32 = 1_000;
const MAX_RESULT_SCALE: u32 = 2_000;

#[derive(Debug, Clone)]
enum DecimalRepr {
    Finite { coefficient: BigInt, scale: u32 },
    NegativeInfinity,
    PositiveInfinity,
    NaN,
}

/// Exact value for `PostgreSQL` `NUMERIC` / `DECIMAL`, including its three
/// special values. Finite values retain their display scale while equality,
/// ordering, hashing keys, and arithmetic operate on the exact value.
#[derive(Debug, Clone)]
pub struct DecimalValue {
    repr: Box<DecimalRepr>,
}

impl DecimalValue {
    fn with_repr(repr: DecimalRepr) -> Self {
        Self {
            repr: Box::new(repr),
        }
    }

    fn repr(&self) -> &DecimalRepr {
        self.repr.as_ref()
    }

    pub fn parse(input: &str) -> Option<Self> {
        let input = input.trim();
        match input.to_ascii_lowercase().as_str() {
            "nan" => return Some(Self::nan()),
            "infinity" | "+infinity" | "inf" | "+inf" => {
                return Some(Self::positive_infinity());
            }
            "-infinity" | "-inf" => return Some(Self::negative_infinity()),
            _ => {}
        }

        let (negative, unsigned) = if let Some(rest) = input.strip_prefix('-') {
            (true, rest)
        } else if let Some(rest) = input.strip_prefix('+') {
            (false, rest)
        } else {
            (false, input)
        };
        let mut exponent_parts = unsigned.split(['e', 'E']);
        let significand = exponent_parts.next()?;
        let exponent_text = exponent_parts.next();
        if exponent_parts.next().is_some() {
            return None;
        }
        let exponent = match exponent_text {
            Some(text) if !text.is_empty() => text.parse::<i32>().ok()?,
            Some(_) => return None,
            None => 0,
        };

        let mut decimal_parts = significand.split('.');
        let integer = decimal_parts.next()?;
        let fractional = decimal_parts.next().unwrap_or("");
        if decimal_parts.next().is_some()
            || (integer.is_empty() && fractional.is_empty())
            || !integer.bytes().all(|byte| byte.is_ascii_digit())
            || !fractional.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let fractional_len = i64::try_from(fractional.len()).ok()?;
        let scale = fractional_len.checked_sub(i64::from(exponent))?;
        let digits = format!("{integer}{fractional}");
        let mut coefficient = BigInt::parse_bytes(digits.as_bytes(), 10)?;
        if negative && !coefficient.is_zero() {
            coefficient = -coefficient;
        }
        if scale < 0 {
            if coefficient.is_zero() {
                return Self::finite(coefficient, 0);
            }
            let power = u32::try_from(scale.checked_neg()?).ok()?;
            if usize::try_from(power).ok()? > MAX_INTEGER_DIGITS {
                return None;
            }
            coefficient *= pow10(power);
            Self::finite(coefficient, 0)
        } else {
            Self::finite(coefficient, u32::try_from(scale).ok()?)
        }
    }

    pub fn from_i64(value: i64) -> Self {
        Self::with_repr(DecimalRepr::Finite {
            coefficient: BigInt::from(value),
            scale: 0,
        })
    }

    pub fn from_i128(value: i128) -> Option<Self> {
        Self::finite(BigInt::from(value), 0)
    }

    pub fn from_bool(value: bool) -> Self {
        Self::from_i64(i64::from(value))
    }

    pub fn from_f64_lossy(value: f64) -> Option<Self> {
        if value.is_nan() {
            return Some(Self::nan());
        }
        if value == f64::INFINITY {
            return Some(Self::positive_infinity());
        }
        if value == f64::NEG_INFINITY {
            return Some(Self::negative_infinity());
        }
        let parsed = Self::parse(&value.to_string())?;
        if value != 0.0 && parsed.is_zero() {
            return None;
        }
        Some(parsed)
    }

    pub fn is_zero(&self) -> bool {
        matches!(
            self.repr(),
            DecimalRepr::Finite { coefficient, .. } if coefficient.is_zero()
        )
    }

    pub fn is_nan(&self) -> bool {
        matches!(self.repr(), DecimalRepr::NaN)
    }

    pub fn is_positive_infinity(&self) -> bool {
        matches!(self.repr(), DecimalRepr::PositiveInfinity)
    }

    pub fn is_negative_infinity(&self) -> bool {
        matches!(self.repr(), DecimalRepr::NegativeInfinity)
    }

    pub fn is_infinite(&self) -> bool {
        self.is_positive_infinity() || self.is_negative_infinity()
    }

    pub fn is_negative(&self) -> bool {
        self.sign() < 0
    }

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

    /// Divide using `PostgreSQL`'s `select_div_scale()` rule: at least sixteen
    /// significant digits, never less scale than either finite operand, and
    /// midpoint rounding away from zero.
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
        match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => Self::with_repr(DecimalRepr::Finite {
                coefficient: coefficient.abs(),
                scale: *scale,
            }),
            DecimalRepr::NegativeInfinity | DecimalRepr::PositiveInfinity => {
                Self::positive_infinity()
            }
            DecimalRepr::NaN => Self::nan(),
        }
    }

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

    pub fn display_scale(&self) -> Option<u32> {
        match self.repr() {
            DecimalRepr::Finite { scale, .. } => Some(*scale),
            _ => None,
        }
    }

    /// Return a conservative byte charge for the boxed representation and any heap-backed coefficient storage retained by this value.
    pub fn retained_bytes(&self) -> usize {
        let coefficient_bytes = match self.repr() {
            DecimalRepr::Finite { coefficient, .. } => {
                let bits = usize::try_from(coefficient.bits()).unwrap_or(usize::MAX);
                let digits = bits.div_ceil(usize::BITS as usize).max(1);
                digits
                    .saturating_mul(std::mem::size_of::<usize>())
                    .saturating_mul(4)
            }
            _ => 0,
        };
        std::mem::size_of::<DecimalRepr>().saturating_add(coefficient_bytes)
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
        match self.repr() {
            DecimalRepr::NaN => true,
            DecimalRepr::NegativeInfinity | DecimalRepr::PositiveInfinity => false,
            DecimalRepr::Finite {
                coefficient,
                scale: value_scale,
            } => {
                let target = if scale >= 0 {
                    let Ok(scale) = u32::try_from(scale) else {
                        return false;
                    };
                    if scale >= *value_scale {
                        coefficient * pow10(scale - *value_scale)
                    } else {
                        coefficient / pow10(*value_scale - scale)
                    }
                } else {
                    let Some(power) = scale
                        .checked_neg()
                        .and_then(|value| u32::try_from(value).ok())
                        .and_then(|value| value.checked_add(*value_scale))
                    else {
                        return false;
                    };
                    coefficient / pow10(power)
                };
                decimal_digit_count(&target) <= usize::try_from(precision).unwrap_or(usize::MAX)
            }
        }
    }

    pub fn to_sql_string(&self) -> String {
        match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => format_finite(coefficient, *scale),
            DecimalRepr::NegativeInfinity => "-Infinity".into(),
            DecimalRepr::PositiveInfinity => "Infinity".into(),
            DecimalRepr::NaN => "NaN".into(),
        }
    }

    pub fn to_canonical_string(&self) -> String {
        match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => {
                let (coefficient, scale) = canonical_finite_parts(coefficient, *scale);
                format_finite(&coefficient, scale)
            }
            _ => self.to_sql_string(),
        }
    }

    /// Normalized base-10 coefficient and scale. The coefficient is returned
    /// as text because `PostgreSQL` numeric coefficients exceed primitive
    /// integer widths.
    pub fn canonical_parts(&self) -> (String, u32) {
        match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => {
                let (coefficient, scale) = canonical_finite_parts(coefficient, *scale);
                (coefficient.to_string(), scale)
            }
            _ => (self.to_sql_string(), 0),
        }
    }

    pub fn sql_string_len(&self) -> usize {
        self.to_sql_string().len()
    }

    pub fn to_i64_trunc(&self) -> Option<i64> {
        let DecimalRepr::Finite { coefficient, scale } = self.repr() else {
            return None;
        };
        (coefficient / pow10(*scale)).to_i64()
    }

    pub fn to_f64(&self) -> Option<f64> {
        match self.repr() {
            DecimalRepr::NegativeInfinity => Some(f64::NEG_INFINITY),
            DecimalRepr::PositiveInfinity => Some(f64::INFINITY),
            DecimalRepr::NaN => Some(f64::NAN),
            DecimalRepr::Finite { .. } => {
                let value = self.to_sql_string().parse::<f64>().ok()?;
                value.is_finite().then_some(value)
            }
        }
    }

    fn finite(coefficient: BigInt, scale: u32) -> Option<Self> {
        if scale > MAX_FRACTIONAL_DIGITS {
            return None;
        }
        let integer_digits = decimal_digit_count(&coefficient).saturating_sub(scale as usize);
        if integer_digits > MAX_INTEGER_DIGITS {
            return None;
        }
        Some(Self::with_repr(DecimalRepr::Finite { coefficient, scale }))
    }

    fn nan() -> Self {
        Self::with_repr(DecimalRepr::NaN)
    }

    fn positive_infinity() -> Self {
        Self::with_repr(DecimalRepr::PositiveInfinity)
    }

    fn negative_infinity() -> Self {
        Self::with_repr(DecimalRepr::NegativeInfinity)
    }

    fn infinity_with_sign(sign: i8) -> Self {
        if sign < 0 {
            Self::negative_infinity()
        } else {
            Self::positive_infinity()
        }
    }

    fn sign(&self) -> i8 {
        match self.repr() {
            DecimalRepr::Finite { coefficient, .. } => {
                if coefficient.is_negative() {
                    -1
                } else {
                    i8::from(!coefficient.is_zero())
                }
            }
            DecimalRepr::NegativeInfinity => -1,
            DecimalRepr::PositiveInfinity | DecimalRepr::NaN => 1,
        }
    }

    fn negated(&self) -> Self {
        match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => Self::with_repr(DecimalRepr::Finite {
                coefficient: -coefficient,
                scale: *scale,
            }),
            DecimalRepr::NegativeInfinity => Self::positive_infinity(),
            DecimalRepr::PositiveInfinity => Self::negative_infinity(),
            DecimalRepr::NaN => Self::nan(),
        }
    }

    fn integral_round(&self, rounding: IntegralRounding) -> Self {
        let DecimalRepr::Finite { coefficient, scale } = self.repr() else {
            return self.clone();
        };
        if *scale == 0 {
            return self.clone();
        }
        let divisor = pow10(*scale);
        let quotient = coefficient / &divisor;
        let remainder = coefficient % divisor;
        let coefficient = match rounding {
            IntegralRounding::Trunc if !remainder.is_zero() => quotient,
            IntegralRounding::Ceil if !remainder.is_zero() && coefficient.is_positive() => {
                quotient + 1
            }
            IntegralRounding::Floor if !remainder.is_zero() && coefficient.is_negative() => {
                quotient - 1
            }
            _ => quotient,
        };
        Self::with_repr(DecimalRepr::Finite {
            coefficient,
            scale: 0,
        })
    }

    fn quantize(&self, target_scale: i32, round: bool) -> Option<Self> {
        let DecimalRepr::Finite { coefficient, scale } = self.repr() else {
            return Some(self.clone());
        };
        if target_scale >= 0 {
            let target_scale = u32::try_from(target_scale).ok()?;
            if target_scale > MAX_FRACTIONAL_DIGITS {
                return None;
            }
            if target_scale >= *scale {
                return Self::finite(coefficient * pow10(target_scale - *scale), target_scale);
            }
            let divisor = pow10(*scale - target_scale);
            let coefficient = if round {
                divide_round_away_from_zero(coefficient, &divisor)?
            } else {
                coefficient / divisor
            };
            return Self::finite(coefficient, target_scale);
        }

        let integer_power = u32::try_from(target_scale.checked_neg()?).ok()?;
        let divisor_power = scale.checked_add(integer_power)?;
        let divisor = pow10(divisor_power);
        let rounded = if round {
            divide_round_away_from_zero(coefficient, &divisor)?
        } else {
            coefficient / divisor
        };
        Self::finite(rounded * pow10(integer_power), 0)
    }
}

#[derive(Clone, Copy)]
enum IntegralRounding {
    Ceil,
    Floor,
    Trunc,
}

fn align_coefficient(coefficient: &BigInt, source_scale: u32, target_scale: u32) -> BigInt {
    coefficient * pow10(target_scale - source_scale)
}

fn pow10(power: u32) -> BigInt {
    BigInt::from(10_u8).pow(power)
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

fn decimal_digit_count(coefficient: &BigInt) -> usize {
    if coefficient.is_zero() {
        1
    } else {
        coefficient.abs().to_str_radix(10).len()
    }
}

fn canonical_finite_parts(coefficient: &BigInt, scale: u32) -> (BigInt, u32) {
    if coefficient.is_zero() {
        return (BigInt::zero(), 0);
    }
    let mut coefficient = coefficient.clone();
    let mut scale = scale;
    while scale > 0 && (&coefficient % 10_u8).is_zero() {
        coefficient /= 10_u8;
        scale -= 1;
    }
    (coefficient, scale)
}

fn format_finite(coefficient: &BigInt, scale: u32) -> String {
    let negative = coefficient.is_negative();
    let digits = coefficient.abs().to_str_radix(10);
    let sign = if negative { "-" } else { "" };
    if scale == 0 {
        return format!("{sign}{digits}");
    }
    let scale = scale as usize;
    if digits.len() > scale {
        let split = digits.len() - scale;
        format!("{sign}{}.{}", &digits[..split], &digits[split..])
    } else {
        format!("{sign}0.{}{digits}", "0".repeat(scale - digits.len()))
    }
}

fn divide_round_away_from_zero(numerator: &BigInt, denominator: &BigInt) -> Option<BigInt> {
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

/// Return `PostgreSQL` `NumericVar`'s normalized base-10000 weight, first
/// non-zero digit, and display scale.
fn numeric_group_head(value: &DecimalValue) -> Option<(i32, u32, u32)> {
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

fn rounded_decimal_product(
    left: &DecimalValue,
    right: &DecimalValue,
    result_scale: i32,
) -> Option<DecimalValue> {
    left.checked_mul(right)?
        .round_to_scale(result_scale.clamp(0, i32::try_from(MAX_FRACTIONAL_DIGITS).ok()?))
}

fn select_decimal_power_scale(
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

impl DecimalValue {
    fn approximate_log10_abs(&self) -> Option<f64> {
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

fn compare_finite(left: &BigInt, left_scale: u32, right: &BigInt, right_scale: u32) -> Ordering {
    let scale = left_scale.max(right_scale);
    align_coefficient(left, left_scale, scale).cmp(&align_coefficient(right, right_scale, scale))
}

impl PartialEq for DecimalValue {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for DecimalValue {}

impl PartialOrd for DecimalValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DecimalValue {
    fn cmp(&self, other: &Self) -> Ordering {
        use DecimalRepr::{Finite, NaN, NegativeInfinity, PositiveInfinity};
        match (self.repr(), other.repr()) {
            (NaN, NaN)
            | (NegativeInfinity, NegativeInfinity)
            | (PositiveInfinity, PositiveInfinity) => Ordering::Equal,
            (NaN, _) => Ordering::Greater,
            (_, NaN) => Ordering::Less,
            (PositiveInfinity, _) => Ordering::Greater,
            (_, PositiveInfinity) | (NegativeInfinity, _) => Ordering::Less,
            (_, NegativeInfinity) => Ordering::Greater,
            (
                Finite {
                    coefficient: left,
                    scale: left_scale,
                },
                Finite {
                    coefficient: right,
                    scale: right_scale,
                },
            ) => compare_finite(left, *left_scale, right, *right_scale),
        }
    }
}

impl Serialize for DecimalValue {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct TaggedDecimal<'a> {
            #[serde(rename = "$uqa_type")]
            kind: &'a str,
            value: String,
        }

        TaggedDecimal {
            kind: "decimal",
            value: self.to_sql_string(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DecimalValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct TaggedDecimal {
            #[serde(rename = "$uqa_type")]
            kind: String,
            value: String,
        }

        let tagged = TaggedDecimal::deserialize(deserializer)?;
        if tagged.kind != "decimal" {
            return Err(serde::de::Error::custom("not a decimal value"));
        }
        Self::parse(&tagged.value).ok_or_else(|| serde::de::Error::custom("invalid decimal value"))
    }
}
