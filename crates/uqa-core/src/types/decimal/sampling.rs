//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Uniform decimal-range sampling with `PostgreSQL`'s base-10000 algorithm.

use num_bigint::{BigInt, BigUint};
use num_traits::{ToPrimitive, Zero};

use super::{align_coefficient, DecimalRepr, DecimalValue};

impl DecimalValue {
    /// Sample uniformly from the inclusive finite decimal range using raw random words supplied by the caller. The result retains the larger display scale of the two bounds, matching `PostgreSQL`'s `random(numeric, numeric)` contract. `Ok(None)` reports non-finite, reversed, or unrepresentable bounds without coupling this value type to SQL error reporting.
    pub fn uniform_sample_inclusive_with<E>(
        &self,
        upper: &Self,
        mut next_u64: impl FnMut() -> Result<u64, E>,
    ) -> Result<Option<Self>, E> {
        let (
            DecimalRepr::Finite {
                coefficient: lower_coefficient,
                scale: lower_scale,
            },
            DecimalRepr::Finite {
                coefficient: upper_coefficient,
                scale: upper_scale,
            },
        ) = (self.repr(), upper.repr())
        else {
            return Ok(None);
        };
        let scale = (*lower_scale).max(*upper_scale);
        let lower = align_coefficient(lower_coefficient, *lower_scale, scale);
        let upper = align_coefficient(upper_coefficient, *upper_scale, scale);
        if lower > upper {
            return Ok(None);
        }
        let span = &upper - &lower;
        if span.is_zero() {
            return Ok(Self::finite(lower, scale));
        }
        let Some(span) = span.to_biguint() else {
            return Ok(None);
        };
        let offset = sample_postgres_numeric_offset(&span, scale, &mut next_u64)?;
        Ok(Self::finite(lower + BigInt::from(offset), scale))
    }
}

fn sample_postgres_numeric_offset<E>(
    span: &BigUint,
    scale: u32,
    next_u64: &mut impl FnMut() -> Result<u64, E>,
) -> Result<BigUint, E> {
    let fractional_groups = scale.div_ceil(4);
    let padding = fractional_groups * 4 - scale;
    let power_of_ten = 10_u64.pow(padding);
    let padded_span = span * BigUint::from(power_of_ten);
    let span_digits = base_10_000_digits(padded_span.clone());
    let digit_count = span_digits.len();
    let prefix_count = digit_count.min(4);
    let prefix = span_digits
        .iter()
        .take(prefix_count)
        .fold(0_u64, |value, digit| value * 10_000 + u64::from(*digit));
    loop {
        let mut digits = vec![0_u16; digit_count];
        let mut random = if prefix_count == digit_count && power_of_ten != 1 {
            random_u64_inclusive(prefix / power_of_ten, next_u64)? * power_of_ten
        } else {
            random_u64_inclusive(prefix, next_u64)?
        };
        for index in (0..prefix_count).rev() {
            digits[index] = u16::try_from(random % 10_000).expect("base-10000 digit must fit u16");
            random /= 10_000;
        }
        let whole_digit_count = digit_count - usize::from(power_of_ten != 1);
        let mut index = prefix_count;
        while index + 4 <= whole_digit_count {
            random = random_u64_inclusive(9_999_999_999_999_999, next_u64)?;
            for digit in &mut digits[index..index + 4] {
                *digit = u16::try_from(random % 10_000).unwrap_or(0);
                random /= 10_000;
            }
            index += 4;
        }
        while index < whole_digit_count {
            digits[index] = u16::try_from(random_u64_inclusive(9_999, next_u64)?)
                .expect("base-10000 digit must fit u16");
            index += 1;
        }
        if index < digit_count {
            let partial = random_u64_inclusive(10_000 / power_of_ten - 1, next_u64)?;
            digits[index] = u16::try_from(partial * power_of_ten)
                .expect("partial base-10000 digit must fit u16");
        }
        let candidate = digits.into_iter().fold(BigUint::ZERO, |value, digit| {
            value * 10_000_u32 + BigUint::from(digit)
        });
        if candidate <= padded_span {
            return Ok(candidate / power_of_ten);
        }
    }
}

fn random_u64_inclusive<E>(
    maximum: u64,
    next_u64: &mut impl FnMut() -> Result<u64, E>,
) -> Result<u64, E> {
    if maximum == 0 {
        return Ok(0);
    }
    let shift = maximum.leading_zeros();
    loop {
        let candidate = next_u64()? >> shift;
        if candidate <= maximum {
            return Ok(candidate);
        }
    }
}

fn base_10_000_digits(mut value: BigUint) -> Vec<u16> {
    let mut digits = Vec::new();
    while !value.is_zero() {
        digits.push(
            (&value % 10_000_u32)
                .to_u16()
                .expect("base-10000 digit must fit u16"),
        );
        value /= 10_000_u32;
    }
    digits.reverse();
    digits
}
