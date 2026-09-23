//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Limb allocation bounds audited against num-bigint 0.4.6 and its native-width digit configuration.

use crate::memory::MemoryError;

type Result<T> = std::result::Result<T, MemoryError>;

pub(super) fn sum(values: &[usize]) -> Result<usize> {
    values.iter().try_fold(0_usize, |total, value| {
        total.checked_add(*value).ok_or(MemoryError::SizeOverflow)
    })
}

fn plus(value: usize, additional: usize) -> Result<usize> {
    value
        .checked_add(additional)
        .ok_or(MemoryError::SizeOverflow)
}

pub(super) fn retained(limbs: usize) -> Result<usize> {
    // BigUint::normalize shrinks when length is below capacity / 4. Integer division leaves three additional limbs at the boundary; include them with DecimalValue's normalized coefficient reservation and minimum native Vec growth.
    limbs
        .max(1)
        .checked_mul(4)
        .and_then(|limbs| limbs.checked_add(3))
        .ok_or(MemoryError::SizeOverflow)
}

pub(super) fn addition(left: usize, right: usize) -> Result<usize> {
    // bigint/{addition,subtraction}.rs forwards borrowed magnitudes to one clone. biguint/addition.rs may extend/push one carry, with an old clone and at most double-capacity replacement; subtraction only normalizes. Four result limbs per possible output limb cover both overlap and the normalized result contract.
    retained(plus(left.max(right), 1)?)
}

pub(super) fn division(dividend: usize, divisor: usize) -> Result<usize> {
    // biguint/division.rs::div_rem_ref and div_rem_core allocate the shifted/cloned dividend, shifted divisor and quotient. The remainder reuses the dividend; normalization may temporarily retain its old buffer while shrinking. The scalar/less/equal branches use subsets of these four slots. A shift by fewer than BigDigit::BITS adds at most one limb.
    let dividend = retained(plus(dividend, 1)?)?;
    let divisor = retained(plus(divisor, 1)?)?;
    sum(&[dividend, divisor, dividend, dividend])
}

pub(super) fn multiplication(left: usize, right: usize) -> Result<usize> {
    let product = plus(plus(left, right)?, 1)?;
    // mul3 allocates product limbs, and normalize may overlap a shrunken result with that buffer. The retained result bound is also covered even when no shrinking occurs.
    sum(&[retained(product)?, mac(left, right)?])
}

fn mac(left: usize, right: usize) -> Result<usize> {
    let (short, long) = if left < right {
        (left, right)
    } else {
        (right, left)
    };
    if short <= 32 {
        // mac_digit and add2 only mutate the caller's product slice.
        return Ok(0);
    }
    // Leading zero limbs can change which branch mac3 takes. Half-Karatsuba itself owns no buffers and visits its children sequentially; the largest allocating descendant has long < 2 * short. Bound that descendant, including every shape produced by trimming, instead of assuming the untrimmed pair selects the same kernel branch.
    let long = long.min(plus(short, short)?.saturating_sub(1));
    if short <= 256 {
        let split = short / 2;
        let high_left = short - split;
        let high_right = long - split;
        let product = plus(plus(high_left, high_right)?, 1)?;
        // The reusable p product plus j0 and j1 differences are the only local heap owners. Each slot includes normalization replacement overlap; children run sequentially against the existing p or accumulator slice.
        return sum(&[
            retained(product)?,
            retained(high_left)?,
            retained(high_right)?,
            mac(high_left, high_right)?,
        ]);
    }
    toom(long)
}

fn toom(long: usize) -> Result<usize> {
    let chunk = plus(long / 3, 1)?;
    let sum_width = plus(chunk, 1)?;
    let argument_width = plus(chunk, 2)?;
    let product_width = plus(plus(chunk, chunk)?, 3)?;
    let interpolation_width = plus(product_width, 1)?;
    // mac3's Toom-3 branch has the following coefficient slots. Consumed slots deliberately stay charged in this bound: this avoids relying on Rust temporary lifetime shortening. Six input parts; p/q/p2/q2; r0/r4/r1/r2/r3; comp1/comp2/comp3; the two r3 input expressions; two interpolation expression temporaries. Coefficient bit growth is at most one limb for additions, two for the doubled r3 arguments, and four above twice a chunk through interpolation.
    let slots = [
        chunk,
        chunk,
        chunk,
        chunk,
        chunk,
        chunk,
        sum_width,
        sum_width,
        sum_width,
        sum_width,
        product_width,
        product_width,
        product_width,
        product_width,
        product_width,
        interpolation_width,
        interpolation_width,
        interpolation_width,
        argument_width,
        argument_width,
        interpolation_width,
        interpolation_width,
    ];
    let local = slots
        .iter()
        .try_fold(0_usize, |total, width| plus(total, retained(*width)?))?;
    // One nested product or one add/subtract/shift replacement runs at a time. Owned scalar division by 2/3 reuses its buffer. Counting both alternatives is conservative and covers all expression temporary overlaps without replacing the numerical kernel.
    sum(&[
        local,
        multiplication(argument_width, argument_width)?,
        addition(interpolation_width, interpolation_width)?,
    ])
}
