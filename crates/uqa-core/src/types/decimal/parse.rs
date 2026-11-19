//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed decimal validation precedes controlled coefficient construction.

use num_bigint::{BigInt, Sign};

use super::{coefficient::Coefficient, DecimalRepr, DecimalValue};
use crate::{
    memory::{Budgeted, MemoryBudget, MemoryError, MemoryReservation, Produced, ProductionControl},
    CancellationToken, ValueRetentionError,
};

mod plan;
use crate::types::work::{checkpoint, inline, Work};
use plan::{FinitePlan, Plan};

type ParseResult<E = ValueRetentionError> =
    Result<Option<(DecimalValue, Option<MemoryReservation>)>, E>;

impl DecimalValue {
    pub fn parse(input: &str) -> Option<Self> {
        parse_with(input, None, &mut || Ok(()))
            .ok()?
            .map(|(value, _)| value)
    }

    pub fn parse_with_control(
        input: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        parse_with(input, control.budget(), &mut || control.check())?
            .map(|(value, memory)| control.finish(value, memory))
            .transpose()
    }

    pub(crate) async fn parse_with_work<W: Work>(
        input: &str,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<Produced<Self>>, W::Error> {
        parse_native(input, control.budget(), &mut || control.check(), work)
            .await?
            .map(|(value, memory)| control.finish(value, memory).map_err(Into::into))
            .transpose()
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
    check: &mut (impl FnMut() -> Result<(), ValueRetentionError> + Send),
) -> ParseResult {
    inline(parse_native(input, budget, check, &mut ()))
}

async fn parse_native<W: Work>(
    input: &str,
    budget: Option<&MemoryBudget>,
    check: &mut (impl FnMut() -> Result<(), ValueRetentionError> + Send),
    work: &mut W,
) -> ParseResult<W::Error> {
    checkpoint(work, check).await?;
    let Some(plan) = Plan::parse(input, check, work).await? else {
        return Ok(None);
    };
    checkpoint(work, check).await?;
    let bytes = match &plan {
        Plan::Special(_) => size_of::<DecimalRepr>(),
        Plan::Finite(plan) => {
            conversion_bytes(plan.digit_count).map_err(ValueRetentionError::from)?
        }
    };
    let mut memory = budget
        .map(|budget| budget.reserve(bytes))
        .transpose()
        .map_err(ValueRetentionError::from)?;
    let value = match plan {
        Plan::Special(repr) => DecimalValue::with_repr(repr),
        Plan::Finite(plan) => {
            let coefficient = coefficient(&plan, &mut memory, check, work).await?;
            checkpoint(work, check).await?;
            // The borrowed plan has already checked the exact decimal precision and scale. Calling finite again would needlessly format the coefficient to rediscover its digit count.
            DecimalValue::with_repr(DecimalRepr::Finite {
                coefficient,
                scale: plan.scale,
            })
        }
    };
    checkpoint(work, check).await?;
    let retained = memory.as_mut().map(|memory| {
        let bytes = value.retained_bytes();
        assert!(bytes <= memory.bytes(), "decimal conversion envelope");
        memory.split(bytes)
    });
    Ok(Some((value, retained)))
}

async fn coefficient<W: Work>(
    plan: &FinitePlan<'_>,
    memory: &mut Option<MemoryReservation>,
    check: &mut (impl FnMut() -> Result<(), ValueRetentionError> + Send),
    work: &mut W,
) -> Result<Coefficient, W::Error> {
    if plan.digit_count == 0 {
        return Ok(Coefficient::ZERO);
    }
    checkpoint(work, check).await?;
    // Every text of at most 38 digits is a word, read without a digit buffer or an arbitrary-precision conversion.
    if plan.digit_count <= 38 {
        let mut magnitude = 0_i128;
        for byte in plan.significand.bytes() {
            if byte != b'.' {
                magnitude = magnitude * 10 + i128::from(byte - b'0');
            }
        }
        for _ in 0..plan.trailing_zeros {
            magnitude *= 10;
        }
        return Ok(Coefficient::Word(if plan.negative {
            -magnitude
        } else {
            magnitude
        }));
    }
    let mut digits = Vec::new();
    digits
        .try_reserve_exact(plan.digit_count)
        .map_err(MemoryError::from)
        .map_err(ValueRetentionError::from)?;
    if let Some(memory) = memory {
        memory
            .grow(digits.capacity().saturating_sub(plan.digit_count))
            .map_err(ValueRetentionError::from)?;
    }
    for (index, byte) in plan.significand.bytes().enumerate() {
        if index.is_multiple_of(4096) {
            checkpoint(work, check).await?;
        }
        if byte != b'.' {
            digits.push(byte - b'0');
        }
    }
    for index in 0..plan.trailing_zeros {
        if index.is_multiple_of(4096) {
            checkpoint(work, check).await?;
        }
        digits.push(0);
    }
    debug_assert_eq!(digits.len(), plan.digit_count);
    checkpoint(work, check).await?;
    let sign = if plan.negative {
        Sign::Minus
    } else {
        Sign::Plus
    };
    let coefficient = BigInt::from_radix_be(sign, &digits, 10).expect("validated decimal digits");
    checkpoint(work, check).await?;
    Ok(Coefficient::from_big(coefficient))
}

fn conversion_bytes(digits: usize) -> Result<usize, MemoryError> {
    if digits <= 38 {
        // A text of at most 38 digits is read into a word coefficient, which retains only its boxed representation.
        return Ok(size_of::<DecimalRepr>());
    }
    // num-bigint 0.4.6, biguint/convert.rs::from_radix_digits_be reserves ceil(log2(10) * digits / BigDigit::BITS) limbs, then updates that one Vec in place. A leading carry slot can grow it once; Vec's minimum non-byte growth is four elements. Reserve both old and replacement buffers, and at least the normalized four-times-limb plus three-limb slack charge. Four bits per decimal digit is an integer upper bound for both 32-bit and 64-bit BigDigit configurations (src/macros.rs). Normalization only truncates a possible leading zero; any shrink also fits the same overlap allowance. No power, coefficient clone, digit string, or formatting buffer is constructed by this path.
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
    let retained = limbs
        .checked_mul(4)
        .and_then(|limbs| limbs.checked_add(3))
        .ok_or(MemoryError::SizeOverflow)?;
    overlap
        .max(retained)
        .checked_mul(limb_bits / 8)
        .and_then(|coefficient| coefficient.checked_add(digits))
        .and_then(|bytes| bytes.checked_add(size_of::<DecimalRepr>()))
        .ok_or(MemoryError::SizeOverflow)
}

#[cfg(test)]
mod tests;
