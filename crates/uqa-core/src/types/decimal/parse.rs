//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed decimal validation precedes controlled coefficient construction.

use num_bigint::{BigInt, Sign};

use super::{DecimalRepr, DecimalValue};
use crate::{
    memory::{Budgeted, MemoryBudget, MemoryError, MemoryReservation},
    CancellationToken, ValueRetentionError,
};

mod plan;
use plan::{FinitePlan, Plan};

type ParseResult = Result<Option<(DecimalValue, Option<MemoryReservation>)>, ValueRetentionError>;

impl DecimalValue {
    pub fn parse(input: &str) -> Option<Self> {
        parse_with(input, None, &mut || Ok(()))
            .ok()?
            .map(|(value, _)| value)
    }

    /// Parse under a shared allowance, reserving decimal digits, coefficient conversion workspace and the boxed result before allocation. Invalid syntax and out-of-range precision return `None`; successful values retain their conservative coefficient charge until dropped.
    pub fn parse_budgeted(
        input: &str,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Option<Budgeted<Self>>, ValueRetentionError> {
        parse_with(input, Some(memory), &mut || {
            cancellation.check().map_err(Into::into)
        })
        .map(|result| {
            result.map(|(value, memory)| {
                Budgeted::new(value, memory.expect("controlled decimal reservation"))
            })
        })
    }
}

fn parse_with(
    input: &str,
    budget: Option<&MemoryBudget>,
    check: &mut impl FnMut() -> Result<(), ValueRetentionError>,
) -> ParseResult {
    check()?;
    let Some(plan) = Plan::parse(input, check)? else {
        return Ok(None);
    };
    check()?;
    let bytes = match &plan {
        Plan::Special(_) => size_of::<DecimalRepr>(),
        Plan::Finite(plan) => conversion_bytes(plan.digit_count)?,
    };
    let mut memory = budget.map(|budget| budget.reserve(bytes)).transpose()?;
    let value = match plan {
        Plan::Special(repr) => DecimalValue::with_repr(repr),
        Plan::Finite(plan) => {
            let coefficient = coefficient(&plan, &mut memory, check)?;
            check()?;
            // The borrowed plan has already checked the exact decimal precision and scale. Calling finite again would needlessly format the coefficient to rediscover its digit count.
            DecimalValue::with_repr(DecimalRepr::Finite {
                coefficient,
                scale: plan.scale,
            })
        }
    };
    check()?;
    let retained = memory.as_mut().map(|memory| {
        let bytes = value.retained_bytes();
        assert!(bytes <= memory.bytes(), "decimal conversion envelope");
        memory.split(bytes)
    });
    Ok(Some((value, retained)))
}

fn coefficient(
    plan: &FinitePlan<'_>,
    memory: &mut Option<MemoryReservation>,
    check: &mut impl FnMut() -> Result<(), ValueRetentionError>,
) -> Result<BigInt, ValueRetentionError> {
    if plan.digit_count == 0 {
        return Ok(BigInt::ZERO);
    }
    check()?;
    let mut digits = Vec::new();
    digits
        .try_reserve_exact(plan.digit_count)
        .map_err(MemoryError::from)?;
    if let Some(memory) = memory {
        memory.grow(digits.capacity().saturating_sub(plan.digit_count))?;
    }
    for (index, byte) in plan.significand.bytes().enumerate() {
        if index.is_multiple_of(4096) {
            check()?;
        }
        if byte != b'.' {
            digits.push(byte - b'0');
        }
    }
    for index in 0..plan.trailing_zeros {
        if index.is_multiple_of(4096) {
            check()?;
        }
        digits.push(0);
    }
    debug_assert_eq!(digits.len(), plan.digit_count);
    check()?;
    let sign = if plan.negative {
        Sign::Minus
    } else {
        Sign::Plus
    };
    let coefficient = BigInt::from_radix_be(sign, &digits, 10).expect("validated decimal digits");
    check()?;
    Ok(coefficient)
}

fn conversion_bytes(digits: usize) -> Result<usize, MemoryError> {
    if digits == 0 {
        // BigInt::ZERO allocates nothing, but keep the same conservative retained representation charge as every other decimal owner.
        return Ok(size_of::<DecimalRepr>() + 4 * size_of::<usize>());
    }
    // num-bigint 0.4.6, biguint/convert.rs::from_radix_digits_be reserves ceil(log2(10) * digits / BigDigit::BITS) limbs, then updates that one Vec in place. A leading carry slot can grow it once; Vec's minimum non-byte growth is four elements. Reserve both old and replacement buffers, and at least the existing four-times-limb retained charge. Four bits per decimal digit is an integer upper bound for both 32-bit and 64-bit BigDigit configurations (src/macros.rs). Normalization only truncates a possible leading zero; any shrink also fits the same overlap allowance. No power, coefficient clone, digit string, or formatting buffer is constructed by this path.
    // The growth rule is audited against Rust 1.90's min_non_zero_cap and grow_amortized: https://github.com/rust-lang/rust/blob/1.90.0/library/alloc/src/raw_vec/mod.rs.
    let limb_bits = if cfg!(target_pointer_width = "64") {
        64_usize
    } else {
        32_usize
    };
    let limbs = digits
        .checked_mul(4)
        .ok_or(MemoryError::SizeOverflow)?
        .div_ceil(limb_bits)
        .max(1);
    let replacement = limbs
        .checked_mul(2)
        .ok_or(MemoryError::SizeOverflow)?
        .max(4);
    let overlap = limbs
        .checked_add(replacement)
        .ok_or(MemoryError::SizeOverflow)?;
    let retained = limbs.checked_mul(4).ok_or(MemoryError::SizeOverflow)?;
    overlap
        .max(retained)
        .checked_mul(limb_bits / 8)
        .and_then(|coefficient| coefficient.checked_add(digits))
        .and_then(|bytes| bytes.checked_add(size_of::<DecimalRepr>()))
        .ok_or(MemoryError::SizeOverflow)
}

#[cfg(test)]
mod tests;
