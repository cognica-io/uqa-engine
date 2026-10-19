//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Arbitrary-precision base-10 values with `PostgreSQL` numeric semantics.

mod arithmetic;
mod arithmetic_control;
mod coefficient;
mod comparison;
mod conversion;
mod format_budgeted;
mod parse;
mod parse_format;
mod power;
mod production;
mod sampling;
mod transcendental;

use num_bigint::BigInt;
use num_traits::Zero;

use coefficient::Coefficient;

const MAX_INTEGER_DIGITS: usize = 131_072;
const MAX_FRACTIONAL_DIGITS: u32 = 16_383;
const MAX_DISPLAY_SCALE: u32 = 1_000;
const MAX_RESULT_SCALE: u32 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
enum DecimalRepr {
    Finite {
        coefficient: Coefficient,
        scale: u32,
    },
    NegativeInfinity,
    PositiveInfinity,
    NaN,
}

/// Exact value for `PostgreSQL` `NUMERIC` / `DECIMAL`, including its three special values. Finite values retain their display scale while equality, ordering, hashing keys, and arithmetic operate on the exact value.
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

    pub(super) fn has_same_representation(&self, other: &Self) -> bool {
        self.repr() == other.repr()
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

    pub fn display_scale(&self) -> Option<u32> {
        match self.repr() {
            DecimalRepr::Finite { scale, .. } => Some(*scale),
            _ => None,
        }
    }

    /// Return a conservative byte charge for the boxed representation and any heap-backed coefficient storage retained by this value.
    pub fn retained_bytes(&self) -> usize {
        let coefficient_bytes = match self.repr() {
            DecimalRepr::Finite { coefficient, .. } => coefficient.retained_bytes(),
            _ => 0,
        };
        std::mem::size_of::<DecimalRepr>().saturating_add(coefficient_bytes)
    }

    fn finite(coefficient: impl Into<Coefficient>, scale: u32) -> Option<Self> {
        if scale > MAX_FRACTIONAL_DIGITS {
            return None;
        }
        let coefficient = coefficient.into();
        let integer_digits = coefficient.digit_count().saturating_sub(scale as usize);
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

    fn sign(&self) -> i8 {
        match self.repr() {
            DecimalRepr::Finite { coefficient, .. } => coefficient.sign(),
            DecimalRepr::NegativeInfinity => -1,
            DecimalRepr::PositiveInfinity | DecimalRepr::NaN => 1,
        }
    }

    fn negated(&self) -> Self {
        match self.repr() {
            DecimalRepr::Finite { coefficient, scale } => Self::with_repr(DecimalRepr::Finite {
                coefficient: coefficient.negated(),
                scale: *scale,
            }),
            DecimalRepr::NegativeInfinity => Self::positive_infinity(),
            DecimalRepr::PositiveInfinity => Self::negative_infinity(),
            DecimalRepr::NaN => Self::nan(),
        }
    }
}

fn align_coefficient(coefficient: &BigInt, source_scale: u32, target_scale: u32) -> BigInt {
    coefficient * pow10(target_scale - source_scale)
}

fn pow10(power: u32) -> BigInt {
    BigInt::from(10_u8).pow(power)
}

/// The coefficient and scale without trailing fractional zeros.
fn canonical_finite_parts(coefficient: &Coefficient, scale: u32) -> (Coefficient, u32) {
    if coefficient.is_zero() {
        return (Coefficient::ZERO, 0);
    }
    let mut scale = scale;
    match coefficient {
        Coefficient::Word(word) => {
            let mut word = *word;
            while scale > 0 && word % 10 == 0 {
                word /= 10;
                scale -= 1;
            }
            (Coefficient::Word(word), scale)
        }
        Coefficient::Big(value) => {
            let mut coefficient = value.clone();
            while scale > 0 && (&coefficient % 10_u8).is_zero() {
                coefficient /= 10_u8;
                scale -= 1;
            }
            (Coefficient::from_big(coefficient), scale)
        }
    }
}
