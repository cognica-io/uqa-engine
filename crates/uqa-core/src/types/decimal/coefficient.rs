//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted workspaces for the existing num-bigint coefficient kernels.

use num_bigint::BigInt;
use num_traits::Signed;

use crate::{
    memory::{MemoryError, Produced, ProductionControl},
    ValueRetentionError,
};

mod workspace;

type Result<T> = std::result::Result<T, ValueRetentionError>;

pub(super) fn add(
    left: &BigInt,
    right: &BigInt,
    control: &ProductionControl<'_>,
) -> Result<Produced<BigInt>> {
    run(
        control,
        || workspace::addition(limbs(left)?, limbs(right)?),
        || left + right,
    )
}

pub(super) fn multiply(
    left: &BigInt,
    right: &BigInt,
    control: &ProductionControl<'_>,
) -> Result<Produced<BigInt>> {
    run(
        control,
        || workspace::multiplication(limbs(left)?, limbs(right)?),
        || left * right,
    )
}

pub(super) fn divide(
    left: &BigInt,
    right: &BigInt,
    control: &ProductionControl<'_>,
) -> Result<Produced<BigInt>> {
    run(
        control,
        || workspace::division(limbs(left)?, limbs(right)?),
        || left / right,
    )
}

pub(super) fn remainder(
    left: &BigInt,
    right: &BigInt,
    control: &ProductionControl<'_>,
) -> Result<Produced<BigInt>> {
    run(
        control,
        || workspace::division(limbs(left)?, limbs(right)?),
        || left % right,
    )
}

pub(super) fn absolute(
    value: &BigInt,
    control: &ProductionControl<'_>,
) -> Result<Produced<BigInt>> {
    run(control, || retained_words(value), || value.abs())
}

pub(super) fn clone(value: &BigInt, control: &ProductionControl<'_>) -> Result<Produced<BigInt>> {
    run(control, || retained_words(value), || value.clone())
}

pub(super) fn from_i64(value: i64, control: &ProductionControl<'_>) -> Result<Produced<BigInt>> {
    run(
        control,
        || {
            let bits = (u64::BITS - value.unsigned_abs().leading_zeros()) as usize;
            workspace::retained(bits.div_ceil(usize::BITS as usize).max(1))
        },
        || BigInt::from(value),
    )
}

pub(super) fn power_of_ten(
    power: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<BigInt>> {
    power_of_small(10, power, control)
}

pub(super) fn power_of_small(
    base: u8,
    power: u32,
    control: &ProductionControl<'_>,
) -> Result<Produced<BigInt>> {
    run(
        control,
        || {
            let bits = usize::try_from(power)
                .map_err(|_| MemoryError::SizeOverflow)?
                .checked_mul((u8::BITS - base.leading_zeros()) as usize)
                .and_then(|bits| bits.checked_add(1))
                .ok_or(MemoryError::SizeOverflow)?;
            let limbs = bits.div_ceil(usize::BITS as usize).max(1);
            // pow_impl keeps the native base and accumulator while one multiply runs. Counting the initial small base as a third retained coefficient also covers the initial clone and replacement overlap. Every intermediate exponent is bounded by the requested exponent; the base's bit width bounds each exponent step.
            workspace::sum(&[
                workspace::retained(limbs)?,
                workspace::retained(limbs)?,
                workspace::retained(1)?,
                workspace::multiplication(limbs, limbs)?,
            ])
        },
        || BigInt::from(base).pow(power),
    )
}

pub(super) fn radix_workspace_words(limbs: usize) -> std::result::Result<usize, MemoryError> {
    let magnitude = workspace::retained(limbs)?;
    if limbs < 64 {
        return Ok(magnitude);
    }
    // to_radix_digits_le builds a base by squaring until floor(sqrt(n)) limbs. This power-of-two root bound works without floating point and covers both the old base and its replacement, including Toom-3 for larger arithmetic intermediates.
    let root_bits = (usize::BITS - limbs.leading_zeros()).div_ceil(2);
    let root = 1_usize
        .checked_shl(root_bits)
        .ok_or(MemoryError::SizeOverflow)?;
    let base = root
        .checked_mul(2)
        .and_then(|limbs| limbs.checked_add(1))
        .ok_or(MemoryError::SizeOverflow)?;
    workspace::sum(&[
        magnitude,
        workspace::retained(base)?,
        workspace::multiplication(root, root)?,
        workspace::division(limbs, base)?,
    ])
}

pub(super) fn retained_bytes(value: &BigInt) -> std::result::Result<usize, MemoryError> {
    retained_words(value)?
        .checked_mul(size_of::<usize>())
        .ok_or(MemoryError::SizeOverflow)
}

fn limbs(value: &BigInt) -> std::result::Result<usize, MemoryError> {
    Ok(usize::try_from(value.bits())
        .map_err(|_| MemoryError::SizeOverflow)?
        .div_ceil(usize::BITS as usize)
        .max(1))
}

fn retained_words(value: &BigInt) -> std::result::Result<usize, MemoryError> {
    workspace::retained(limbs(value)?)
}

fn run(
    control: &ProductionControl<'_>,
    words: impl FnOnce() -> std::result::Result<usize, MemoryError>,
    kernel: impl FnOnce() -> BigInt,
) -> Result<Produced<BigInt>> {
    control.check()?;
    let mut memory = if control.budget().is_some() {
        control.reserve(
            words()?
                .checked_mul(size_of::<usize>())
                .ok_or(MemoryError::SizeOverflow)?,
        )?
    } else {
        None
    };
    // The dependency exposes neither an allocator nor a cancellation callback. Its existing kernel executes between checks; all native Decimal loops also check their control.
    let value = kernel();
    control.check()?;
    let retained = memory.as_mut().map(|memory| {
        let bytes = retained_bytes(&value).expect("admitted coefficient size");
        assert!(
            bytes <= memory.bytes(),
            "coefficient kernel workspace covers normalized result"
        );
        memory.split(bytes)
    });
    control.finish(value, retained)
}

#[cfg(test)]
mod tests;
