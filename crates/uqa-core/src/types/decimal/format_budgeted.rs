//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical decimal text retains a controlled radix-conversion workspace.

use num_traits::{Signed, Zero};

use super::{DecimalRepr, DecimalValue};
use crate::{
    memory::{
        BudgetedString, MemoryBudget, MemoryError, Produced, ProductionControl, ProductionString,
    },
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
        let control = ProductionControl::new(memory, cancellation, cancellation);
        Ok(BudgetedString::from_budgeted(
            self.to_canonical_string_with_control(&control)?
                .into_budgeted()
                .expect("controlled decimal formatter"),
        ))
    }

    pub fn to_canonical_string_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        self.format_with_control(true, control)
    }

    pub fn to_sql_string_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        self.format_with_control(false, control)
    }

    fn format_with_control(
        &self,
        canonical: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        control.check()?;
        let mut output = ProductionString::new(*control);
        let (coefficient, scale) = match self.repr() {
            DecimalRepr::Finite { coefficient, scale } if !canonical || !coefficient.is_zero() => {
                (coefficient, *scale as usize)
            }
            DecimalRepr::Finite { .. } => {
                output.push('0')?;
                return output.finish();
            }
            special => {
                output.push_str(match special {
                    DecimalRepr::NegativeInfinity => "-Infinity",
                    DecimalRepr::PositiveInfinity => "Infinity",
                    DecimalRepr::NaN => "NaN",
                    DecimalRepr::Finite { .. } => unreachable!(),
                })?;
                return output.finish();
            }
        };
        let digits = coefficient_digits_with_control(coefficient, control)?;
        let mut removed = 0;
        if canonical {
            for digit in digits.iter().take(scale) {
                control.check()?;
                if *digit != 0 {
                    break;
                }
                removed += 1;
            }
        }
        let scale = scale - removed;
        let digits = &digits[removed..];
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
            push_digits(&mut output, digits)?;
        } else if digits.len() > scale {
            let split = digits.len() - scale;
            push_digits(&mut output, &digits[digits.len() - split..])?;
            output.push('.')?;
            push_digits(&mut output, &digits[..digits.len() - split])?;
        } else {
            output.push_str("0.")?;
            let mut padding = scale - digits.len();
            while padding != 0 {
                control.check()?;
                let count = padding.min(ZEROS.len());
                output.push_str(&ZEROS[..count])?;
                padding -= count;
            }
            push_digits(&mut output, digits)?;
        }
        control.check()?;
        output.finish()
    }
}

fn push_digits(
    output: &mut ProductionString<'_>,
    digits: &[u8],
) -> Result<(), ValueRetentionError> {
    for digit in digits.iter().rev() {
        output.push(char::from(b'0' + digit))?;
    }
    Ok(())
}

pub(super) fn coefficient_digits_with_control(
    coefficient: &num_bigint::BigInt,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, ValueRetentionError> {
    control.check()?;
    let mut workspace = if control.budget().is_some() {
        control.reserve(radix_workspace_bytes(coefficient.bits())?)?
    } else {
        None
    };
    let digits = coefficient.magnitude().to_radix_le(10);
    if let Some(workspace) = &mut workspace {
        workspace.grow(digits.capacity().saturating_sub(workspace.bytes()))?;
    }
    let memory = workspace
        .as_mut()
        .map(|memory| memory.split(digits.capacity()));
    control.finish(digits, memory)
}

fn radix_workspace_bytes(bits: u64) -> Result<usize, MemoryError> {
    let bits = usize::try_from(bits).map_err(|_| MemoryError::SizeOverflow)?;
    let limbs = bits.div_ceil(usize::BITS as usize).max(1);
    // num-bigint 0.4.6's to_radix_digits_le owns a magnitude clone, its large radix base, multiplication/division workspaces, and a byte result. Reuse the source-audited native kernel plans, so arithmetic intermediates are covered without assuming a final Decimal precision limit. The byte vector has at most ceil(bits / 3) + 1 digits; old plus replacement capacity is at most three times that bound, plus Vec's minimum byte capacity.
    let workspace = super::coefficient::radix_workspace_words(limbs)?
        .checked_mul(size_of::<usize>())
        .ok_or(MemoryError::SizeOverflow)?;
    bits.div_ceil(3)
        .checked_add(1)
        .and_then(|digits| digits.checked_mul(3))
        .and_then(|digits| digits.checked_add(8))
        .and_then(|digits| digits.checked_add(workspace))
        .ok_or(MemoryError::SizeOverflow)
}

#[cfg(test)]
mod tests;
