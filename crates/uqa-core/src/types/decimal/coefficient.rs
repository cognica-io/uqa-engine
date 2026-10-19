//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted workspaces for the existing num-bigint coefficient kernels.

use std::{borrow::Cow, cmp::Ordering, fmt};

use num_bigint::BigInt;
use num_traits::{Signed, Zero};

use crate::{
    memory::{MemoryError, Produced, ProductionControl},
    ValueRetentionError,
};

mod workspace;

type Result<T> = std::result::Result<T, ValueRetentionError>;

/// A finite coefficient: a machine word while the value fits one, so ordinary numeric values hold no heap allocation, and an arbitrary-precision integer beyond that. A `Big` coefficient never holds a value that fits `i128`; every constructor keeps that canonical form, so structural equality of representations is equality of values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Coefficient {
    Word(i128),
    Big(BigInt),
}

impl Coefficient {
    pub(super) const ZERO: Self = Self::Word(0);

    pub(super) fn from_big(value: BigInt) -> Self {
        match i128::try_from(&value) {
            Ok(word) => Self::Word(word),
            Err(_) => Self::Big(value),
        }
    }

    pub(super) fn word(&self) -> Option<i128> {
        match self {
            Self::Word(word) => Some(*word),
            Self::Big(_) => None,
        }
    }

    /// The coefficient as an arbitrary-precision integer, borrowed when it already is one.
    pub(super) fn big(&self) -> Cow<'_, BigInt> {
        match self {
            Self::Word(word) => Cow::Owned(BigInt::from(*word)),
            Self::Big(value) => Cow::Borrowed(value),
        }
    }

    pub(super) fn is_zero(&self) -> bool {
        match self {
            Self::Word(word) => *word == 0,
            Self::Big(value) => value.is_zero(),
        }
    }

    pub(super) fn is_negative(&self) -> bool {
        match self {
            Self::Word(word) => *word < 0,
            Self::Big(value) => value.is_negative(),
        }
    }

    pub(super) fn sign(&self) -> i8 {
        match self {
            Self::Word(word) => word.signum() as i8,
            Self::Big(value) => {
                if value.is_negative() {
                    -1
                } else {
                    i8::from(!value.is_zero())
                }
            }
        }
    }

    pub(super) fn negated(&self) -> Self {
        match self {
            Self::Word(word) => word
                .checked_neg()
                .map_or_else(|| Self::Big(-BigInt::from(*word)), Self::Word),
            Self::Big(value) => Self::from_big(-value),
        }
    }

    pub(super) fn abs(&self) -> Self {
        match self {
            Self::Word(word) => word
                .checked_abs()
                .map_or_else(|| Self::Big(BigInt::from(*word).abs()), Self::Word),
            Self::Big(value) => Self::from_big(value.abs()),
        }
    }

    /// The number of decimal digits of the magnitude; zero has one.
    pub(super) fn digit_count(&self) -> usize {
        match self {
            Self::Word(0) => 1,
            Self::Word(word) => word.unsigned_abs().ilog10() as usize + 1,
            Self::Big(value) => value.abs().to_str_radix(10).len(),
        }
    }

    /// The word coefficient scaled from `source_scale` to `target_scale` in 128-bit arithmetic. `None` leaves the value to the arbitrary-precision kernels.
    pub(super) fn aligned_word(&self, source_scale: u32, target_scale: u32) -> Option<i128> {
        let word = self.word()?;
        let power = target_scale.checked_sub(source_scale)?;
        word.checked_mul(10_i128.checked_pow(power)?)
    }

    /// The bytes a retained coefficient holds beyond its representation: none for a word, the normalized limb allocation for an arbitrary-precision integer.
    pub(super) fn retained_bytes(&self) -> usize {
        match self {
            Self::Word(_) => 0,
            Self::Big(value) => {
                let bits = usize::try_from(value.bits()).unwrap_or(usize::MAX);
                let limbs = bits.div_ceil(usize::BITS as usize).max(1);
                // num-bigint 0.4.6 normalizes when len < capacity / 4. Integer division leaves up to three additional limbs at that boundary.
                limbs
                    .saturating_mul(4)
                    .saturating_add(3)
                    .saturating_mul(size_of::<usize>())
            }
        }
    }

    /// The retained coefficient of a kernel result. A result that fits a word keeps no limb allocation, so the lease its kernel reserved is released.
    pub(super) fn produced(
        value: Produced<BigInt>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        let (value, memory) = value.into_parts();
        match i128::try_from(&value) {
            Ok(word) => {
                drop(memory);
                control.finish(Self::Word(word), control.empty_reservation())
            }
            Err(_) => control.finish(Self::Big(value), memory),
        }
    }

    /// A word result, which retains nothing beyond the representation that holds it.
    pub(super) fn word_produced(
        value: i128,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>> {
        control.check()?;
        control.finish(Self::Word(value), control.empty_reservation())
    }
}

impl From<i128> for Coefficient {
    fn from(value: i128) -> Self {
        Self::Word(value)
    }
}

impl From<i64> for Coefficient {
    fn from(value: i64) -> Self {
        Self::Word(i128::from(value))
    }
}

impl From<BigInt> for Coefficient {
    fn from(value: BigInt) -> Self {
        Self::from_big(value)
    }
}

impl fmt::Display for Coefficient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Word(word) => write!(f, "{word}"),
            Self::Big(value) => write!(f, "{value}"),
        }
    }
}

impl PartialOrd for Coefficient {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Coefficient {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Word(left), Self::Word(right)) => left.cmp(right),
            (Self::Big(left), Self::Big(right)) => left.cmp(right),
            // An arbitrary-precision coefficient lies beyond every word, on the side of its sign.
            (Self::Word(_), Self::Big(right)) => {
                if right.is_negative() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (Self::Big(left), Self::Word(_)) => {
                if left.is_negative() {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
        }
    }
}

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

/// Multiply by one machine word, which needs no power-of-ten coefficient of its own.
pub(super) fn multiply_by_word(
    value: &BigInt,
    factor: u64,
    control: &ProductionControl<'_>,
) -> Result<Produced<BigInt>> {
    run(
        control,
        || workspace::multiplication(limbs(value)?, 1),
        || value * factor,
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
