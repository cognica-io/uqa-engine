//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical decimal text retains a controlled radix-conversion workspace.

use num_traits::{Signed, Zero};

use super::{DecimalRepr, DecimalValue};
use crate::{
    memory::{Budgeted, BudgetedString, MemoryBudget, MemoryError},
    CancellationToken, ValueRetentionError,
};

const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

impl DecimalValue {
    /// Format the existing canonical numeric representation under the caller's allowance. Radix conversion reserves its workspace before allocation; the returned text keeps its own buffer charged.
    pub fn to_canonical_string_budgeted(
        &self,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<BudgetedString, ValueRetentionError> {
        cancellation.check()?;
        let mut output = BudgetedString::new(memory);
        let (coefficient, scale) = match self.repr() {
            DecimalRepr::Finite { coefficient, scale } if !coefficient.is_zero() => {
                (coefficient, *scale as usize)
            }
            DecimalRepr::Finite { .. } => {
                output.push('0')?;
                return Ok(output);
            }
            special => {
                output.push_str(match special {
                    DecimalRepr::NegativeInfinity => "-Infinity",
                    DecimalRepr::PositiveInfinity => "Infinity",
                    DecimalRepr::NaN => "NaN",
                    DecimalRepr::Finite { .. } => unreachable!(),
                })?;
                return Ok(output);
            }
        };
        let mut workspace = memory.reserve(radix_workspace_bytes(coefficient.bits())?)?;
        let mut digits = coefficient.magnitude().to_radix_le(10);
        workspace.grow(digits.capacity().saturating_sub(workspace.bytes()))?;
        // Trimming fractional zero digits avoids cloning and repeatedly dividing the coefficient just to normalize its scale.
        let removed = digits
            .iter()
            .take(scale)
            .take_while(|digit| **digit == 0)
            .count();
        let scale = scale - removed;
        digits.reverse();
        digits.truncate(digits.len() - removed);
        for chunk in digits.chunks_mut(4096) {
            cancellation.check()?;
            for digit in chunk {
                *digit += b'0';
            }
        }
        let digit_memory = workspace.split(digits.capacity());
        let digits = Budgeted::new(digits, digit_memory);
        drop(workspace);
        let digits = std::str::from_utf8(&digits).expect("ASCII decimal digits");
        let length = digits
            .len()
            .max(scale.saturating_add(1))
            .checked_add(usize::from(scale != 0) + usize::from(coefficient.is_negative()))
            .ok_or(MemoryError::SizeOverflow)?;
        output.reserve(length)?;
        if coefficient.is_negative() {
            output.push('-')?;
        }
        if scale == 0 {
            append(&mut output, digits, cancellation)?;
        } else if digits.len() > scale {
            let split = digits.len() - scale;
            append(&mut output, &digits[..split], cancellation)?;
            output.push('.')?;
            append(&mut output, &digits[split..], cancellation)?;
        } else {
            output.push_str("0.")?;
            let mut padding = scale - digits.len();
            while padding != 0 {
                cancellation.check()?;
                let count = padding.min(ZEROS.len());
                output.push_str(&ZEROS[..count])?;
                padding -= count;
            }
            append(&mut output, digits, cancellation)?;
        }
        cancellation.check()?;
        Ok(output)
    }
}

fn append(
    output: &mut BudgetedString,
    digits: &str,
    cancellation: &CancellationToken,
) -> Result<(), ValueRetentionError> {
    for chunk in digits.as_bytes().chunks(4096) {
        cancellation.check()?;
        output.push_str(std::str::from_utf8(chunk).expect("ASCII decimal digits"))?;
    }
    Ok(())
}

fn radix_workspace_bytes(bits: u64) -> Result<usize, MemoryError> {
    let bits = usize::try_from(bits).map_err(|_| MemoryError::SizeOverflow)?;
    let limbs = bits.div_ceil(usize::BITS as usize).max(1);
    // num-bigint 0.4.6's to_radix_digits_le clones the magnitude and reserves the digit output. Below 64 limbs it divides that clone in place. The large path retains a base of at most twice sqrt(n) limbs and performs Knuth division: old magnitude, shifted operands (including old/new growth overlap), quotient and remainder-normalization overlap fit within 16 * (n + 4) limbs. Base construction only uses long/Karatsuba multiplication: PostgreSQL's 131072 integer + 16383 fractional digits give fewer than 18432 32-bit limbs, hence squaring operands below 136 limbs (the Toom-3 threshold is 256). Karatsuba's geometrically shrinking product/difference scratch fits the same envelope. The byte output has fewer than ceil(bits / 3) digits; double that size plus Vec's minimum byte capacity covers growth overlap. No coefficient normalization, signed copy or string formatting is performed inside this reservation.
    let workspace = limbs
        .checked_add(4)
        .and_then(|limbs| limbs.checked_mul(if limbs < 68 { 4 } else { 16 }))
        .and_then(|limbs| limbs.checked_mul(size_of::<usize>()))
        .ok_or(MemoryError::SizeOverflow)?;
    bits.div_ceil(3)
        .checked_add(1)
        .and_then(|digits| digits.checked_mul(2))
        .and_then(|digits| digits.checked_add(8))
        .and_then(|digits| digits.checked_add(workspace))
        .ok_or(MemoryError::SizeOverflow)
}

#[cfg(test)]
mod tests;
