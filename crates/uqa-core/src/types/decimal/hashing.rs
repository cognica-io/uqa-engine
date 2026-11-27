//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact numeric hashing across binary and decimal representations.

use std::hash::{Hash, Hasher};

use num_bigint::BigInt;

use super::{
    coefficient::Coefficient, conversion::finite_float_parts, DecimalRepr, MAX_FRACTIONAL_DIGITS,
};
use crate::Value;

// One fractional digit in this radix fits in a u64.
const FIVES_PER_LIMB: u32 = 27;
const RADIX: u64 = 5_u64.pow(FIVES_PER_LIMB);
const MAX_FRACTION_LIMBS: usize = MAX_FRACTIONAL_DIGITS.div_ceil(FIVES_PER_LIMB) as usize;

enum Magnitude<'a> {
    Word(u128),
    Big(&'a BigInt),
}

enum NumericHash<'a> {
    Zero,
    Finite {
        negative: bool,
        magnitude: Magnitude<'a>,
        twos: u64,
        binary_exponent: i64,
        five_scale: u32,
    },
    NegativeInfinity,
    PositiveInfinity,
    Nan,
}

impl Value {
    /// Borrow the exact numeric hash key shared by equal booleans, integers, floats and decimals.
    pub fn numeric_hash_key(&self) -> Option<impl Hash + '_> {
        Some(match self {
            Self::Bool(value) => word(false, u128::from(*value), 0, 0),
            Self::Int(value) => word(value.is_negative(), u128::from(value.unsigned_abs()), 0, 0),
            Self::Float(value) if value.is_nan() => NumericHash::Nan,
            Self::Float(value) if *value == f64::NEG_INFINITY => NumericHash::NegativeInfinity,
            Self::Float(value) if *value == f64::INFINITY => NumericHash::PositiveInfinity,
            Self::Float(value) => {
                let (mantissa, exponent) = finite_float_parts(*value);
                word(
                    value.is_sign_negative(),
                    u128::from(mantissa),
                    i64::from(exponent),
                    0,
                )
            }
            Self::Decimal(value) => match value.repr() {
                DecimalRepr::NaN => NumericHash::Nan,
                DecimalRepr::NegativeInfinity => NumericHash::NegativeInfinity,
                DecimalRepr::PositiveInfinity => NumericHash::PositiveInfinity,
                DecimalRepr::Finite { coefficient, scale } => match coefficient {
                    Coefficient::Word(value) => word(
                        value.is_negative(),
                        value.unsigned_abs(),
                        -i64::from(*scale),
                        *scale,
                    ),
                    Coefficient::Big(value) => {
                        let twos = value.trailing_zeros().expect("nonzero large coefficient");
                        NumericHash::Finite {
                            negative: coefficient.is_negative(),
                            magnitude: Magnitude::Big(value),
                            twos,
                            binary_exponent: i64::try_from(twos)
                                .expect("bounded numeric coefficient")
                                - i64::from(*scale),
                            five_scale: *scale,
                        }
                    }
                },
            },
            _ => return None,
        })
    }
}

fn word(negative: bool, value: u128, exponent: i64, five_scale: u32) -> NumericHash<'static> {
    if value == 0 {
        return NumericHash::Zero;
    }
    let twos = value.trailing_zeros();
    NumericHash::Finite {
        negative,
        magnitude: Magnitude::Word(value >> twos),
        twos: 0,
        binary_exponent: exponent + i64::from(twos),
        five_scale,
    }
}

impl Hash for NumericHash<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        let Self::Finite {
            negative,
            magnitude,
            twos,
            binary_exponent,
            five_scale,
        } = self
        else {
            return;
        };
        negative.hash(state);
        binary_exponent.hash(state);
        match magnitude {
            Magnitude::Word(value) => {
                hash_magnitude(
                    [(*value >> 64) as u64, *value as u64].into_iter(),
                    *five_scale,
                    state,
                );
            }
            Magnitude::Big(value) => {
                let digits = value.iter_u64_digits();
                let count = digits.len() - usize::try_from(twos / 64).expect("bounded coefficient");
                let shift = (twos % 64) as u32;
                let mut carry = 0;
                let odd = digits.rev().take(count).map(|digit| {
                    if shift == 0 {
                        return digit;
                    }
                    let output = (digit >> shift) | carry;
                    carry = digit << (64 - shift);
                    output
                });
                hash_magnitude(odd, *five_scale, state);
            }
        }
    }
}

// A finite value is sign * 2^exponent * odd / 5^scale. Hash the quotient's integer limbs and its exact, radix-point-aligned fractional limbs. Extra display zeros produce the same digits.
fn hash_magnitude(digits: impl Iterator<Item = u64>, scale: u32, state: &mut impl Hasher) {
    let groups = scale.div_ceil(FIVES_PER_LIMB) as usize;
    match groups {
        0 => hash_fraction(digits, 1, &mut [], state),
        1 => hash_fraction(digits, 5_u64.pow(scale), &mut [0], state),
        _ => {
            let tail_width = (scale - 1) % FIVES_PER_LIMB + 1;
            hash_long_fraction(digits, groups, 5_u64.pow(tail_width), state);
        }
    }
}

// The workspace is bounded by the SQL fractional scale, independently of the coefficient's integer precision. Keep it off the common integer and single-group paths.
#[inline(never)]
fn hash_long_fraction(
    digits: impl Iterator<Item = u64>,
    groups: usize,
    tail_radix: u64,
    state: &mut impl Hasher,
) {
    let mut remainders = [0; MAX_FRACTION_LIMBS];
    hash_fraction(digits, tail_radix, &mut remainders[..groups], state);
}

fn divide_limb(digit: u64, remainder: &mut u64, radix: u64) -> u64 {
    let pair = (u128::from(*remainder) << 64) | u128::from(digit);
    *remainder = (pair % u128::from(radix)) as u64;
    (pair / u128::from(radix)) as u64
}

fn hash_fraction(
    digits: impl Iterator<Item = u64>,
    tail_radix: u64,
    remainders: &mut [u64],
    state: &mut impl Hasher,
) {
    let mut integer_len = 0_u32;
    let mut used_whole_groups = 0;
    for mut digit in digits {
        if let Some((tail, whole)) = remainders.split_first_mut() {
            digit = divide_limb(digit, tail, tail_radix);
            for (index, remainder) in whole.iter_mut().enumerate() {
                if index >= used_whole_groups && digit == 0 {
                    break;
                }
                digit = divide_limb(digit, remainder, RADIX);
                used_whole_groups = used_whole_groups.max(index + 1);
            }
        }
        if digit != 0 || integer_len > 0 {
            digit.hash(state);
            integer_len += 1;
        }
    }
    if let Some(tail) = remainders.first_mut() {
        *tail *= RADIX / tail_radix;
    }
    let first = remainders
        .iter()
        .position(|&part| part != 0)
        .unwrap_or(remainders.len());
    let fraction = &remainders[first..];
    for part in fraction.iter().rev() {
        part.hash(state);
    }
    integer_len.hash(state);
    fraction.len().hash(state);
}

#[cfg(test)]
mod tests;
