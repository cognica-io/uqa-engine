//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory hashing of exact numeric values across their physical representations.

use std::hash::Hash;

use super::{coefficient::Coefficient, conversion::finite_float_parts, DecimalRepr};
use crate::Value;

// 2^64 - 59 is prime. Both denominator factors of a finite decimal have inverses.
const PRIME: u64 = u64::MAX - 58;
const INVERSE_TWO: u64 = PRIME / 2 + 1;
const INVERSE_FIVE: u64 = ((2 * PRIME as u128 + 1) / 5) as u64;

#[derive(Hash)]
enum NumericHash {
    Zero,
    Finite {
        negative: bool,
        binary_exponent: i64,
        odd_residue: u64,
    },
    NegativeInfinity,
    PositiveInfinity,
    Nan,
}

impl Value {
    /// Return a hashable fingerprint in the exact numeric equality domain. Equal bools, integers, floats and decimals hash alike. Hash table lookups still compare the original values.
    pub fn numeric_hash_key(&self) -> Option<impl Hash> {
        Some(match self {
            Self::Bool(value) => binary(false, u64::from(*value), 0),
            Self::Int(value) => binary(value.is_negative(), value.unsigned_abs(), 0),
            Self::Float(value) if value.is_nan() => NumericHash::Nan,
            Self::Float(value) if *value == f64::NEG_INFINITY => NumericHash::NegativeInfinity,
            Self::Float(value) if *value == f64::INFINITY => NumericHash::PositiveInfinity,
            Self::Float(value) => {
                let (mantissa, exponent) = finite_float_parts(*value);
                binary(value.is_sign_negative(), mantissa, exponent)
            }
            Self::Decimal(value) => match value.repr() {
                DecimalRepr::NaN => NumericHash::Nan,
                DecimalRepr::NegativeInfinity => NumericHash::NegativeInfinity,
                DecimalRepr::PositiveInfinity => NumericHash::PositiveInfinity,
                DecimalRepr::Finite { coefficient, scale } => decimal(coefficient, *scale),
            },
            _ => return None,
        })
    }
}

fn binary(negative: bool, mantissa: u64, exponent: i32) -> NumericHash {
    if mantissa == 0 {
        return NumericHash::Zero;
    }
    let twos = mantissa.trailing_zeros();
    NumericHash::Finite {
        negative,
        binary_exponent: i64::from(exponent) + i64::from(twos),
        odd_residue: mantissa >> twos,
    }
}

// For c / 10^s with t trailing zero bits in |c|, the binary exponent is t - s.
// The remaining odd rational is (|c| / 2^t) / 5^s. Its residue is invariant
// under extra display zeros, and every binary float has denominator one here.
fn decimal(coefficient: &Coefficient, scale: u32) -> NumericHash {
    let (twos, odd) = match coefficient {
        Coefficient::Word(0) => return NumericHash::Zero,
        Coefficient::Word(word) => {
            let magnitude = word.unsigned_abs();
            let twos = magnitude.trailing_zeros();
            (u64::from(twos), reduce(magnitude >> twos))
        }
        Coefficient::Big(value) => {
            let twos = value.trailing_zeros().expect("nonzero big coefficient");
            let magnitude = value.iter_u64_digits().rev().fold(0_u64, |high, low| {
                reduce((u128::from(high) << 64) | u128::from(low))
            });
            (twos, scale_residue(magnitude, INVERSE_TWO, twos))
        }
    };
    NumericHash::Finite {
        negative: coefficient.is_negative(),
        binary_exponent: i64::try_from(twos).expect("bounded numeric coefficient")
            - i64::from(scale),
        odd_residue: scale_residue(odd, INVERSE_FIVE, u64::from(scale)),
    }
}

fn reduce(value: u128) -> u64 {
    (value % u128::from(PRIME)) as u64
}

fn scale_residue(mut residue: u64, mut factor: u64, mut exponent: u64) -> u64 {
    while exponent != 0 {
        if exponent & 1 != 0 {
            residue = reduce(u128::from(residue) * u128::from(factor));
        }
        exponent >>= 1;
        if exponent != 0 {
            factor = reduce(u128::from(factor) * u128::from(factor));
        }
    }
    residue
}
