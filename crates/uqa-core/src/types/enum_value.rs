//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! User-defined enum values ordered by immutable label keys.
//!
//! A label key is a nonempty byte string without a trailing zero byte. Reading
//! it as the base-256 fraction `0.k1 k2 ... kn` maps distinct keys to distinct
//! rationals in `(0, 1)`, and lexicographic byte order equals the order of those
//! fractions. Allocation only ever returns a key strictly between the keys of
//! the new label's neighbors and never changes an existing key, so key order is
//! the enum's declaration order for the whole lifetime of the type.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Upper bound for one encoded label key. Allocation grows a key by at most one
/// byte for every eight insertions at the same position, so the bound is only
/// reachable by pathological histories and protects every decoder from
/// unbounded stored keys.
pub const MAX_ENUM_LABEL_KEY_BYTES: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnumLabelKeyError {
    #[error("enum label key is empty")]
    Empty,
    #[error("enum label key ends with a zero byte")]
    TrailingZero,
    #[error("enum label key of {0} bytes exceeds the {MAX_ENUM_LABEL_KEY_BYTES}-byte limit")]
    TooLong(usize),
    #[error("enum label key bounds are not strictly increasing")]
    UnorderedBounds,
}

/// Immutable order-preserving identity of one enum label within its type.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EnumLabelKey(Box<[u8]>);

impl EnumLabelKey {
    /// Validate a stored key without normalizing it.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, EnumLabelKeyError> {
        validate(&bytes)?;
        Ok(Self(bytes.into_boxed_slice()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Allocate evenly spaced keys for the labels of a new type in declaration
    /// order. The spacing leaves room on both sides of every label, so later
    /// insertions usually keep one-byte keys.
    pub fn initial(count: usize) -> Result<Vec<Self>, EnumLabelKeyError> {
        let slots = count
            .checked_add(1)
            .ok_or(EnumLabelKeyError::TooLong(usize::MAX))?;
        let mut width = 1_usize;
        // Each fraction is (index + 1) / slots scaled to `width` base-256 digits.
        // Distinct scaled values need at least two units of space per slot so
        // that consecutive labels never share one numerator.
        while !digits_hold(width, slots) {
            width += 1;
            if width > MAX_ENUM_LABEL_KEY_BYTES {
                return Err(EnumLabelKeyError::TooLong(width));
            }
        }
        let mut keys = Vec::with_capacity(count);
        for index in 1..=count {
            keys.push(Self(
                scaled_fraction(index, slots, width).into_boxed_slice(),
            ));
        }
        Ok(keys)
    }

    /// Allocate a key strictly between two neighbors; `None` denotes the open
    /// end of the label list. Appending increments the shortest prefix so that
    /// repeated `ADD VALUE` without a position keeps keys short.
    pub fn between(lower: Option<&Self>, upper: Option<&Self>) -> Result<Self, EnumLabelKeyError> {
        if let (Some(lower), Some(upper)) = (lower, upper) {
            if lower >= upper {
                return Err(EnumLabelKeyError::UnorderedBounds);
            }
        }
        let bytes = match (lower, upper) {
            (Some(lower), None) => increment(lower.as_bytes()),
            (lower, upper) => midpoint(
                lower.map_or(&[][..], Self::as_bytes),
                upper.map(Self::as_bytes),
            )?,
        };
        validate(&bytes)?;
        Ok(Self(bytes.into_boxed_slice()))
    }
}

impl fmt::Debug for EnumLabelKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnumLabelKey(")?;
        for byte in &self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str(")")
    }
}

/// Keys serialize as lowercase hexadecimal text; decoding validates the key invariant so that every storage mirror rejects corruption.
impl Serialize for EnumLabelKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for EnumLabelKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = std::borrow::Cow::<'de, str>::deserialize(deserializer)?;
        Self::from_hex(&text).map_err(serde::de::Error::custom)
    }
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

impl EnumLabelKey {
    /// Lowercase hexadecimal text of the key, the spelling of every serialized carrier.
    pub fn to_hex(&self) -> String {
        let mut text = String::with_capacity(self.0.len() * 2);
        for byte in &self.0 {
            text.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
            text.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
        }
        text
    }

    /// Decode hexadecimal key text and validate the key invariant.
    pub fn from_hex(text: &str) -> Result<Self, EnumLabelKeyParseError> {
        let digits = text.as_bytes();
        if !digits.len().is_multiple_of(2) {
            return Err(EnumLabelKeyParseError::Hexadecimal);
        }
        if digits.len() / 2 > MAX_ENUM_LABEL_KEY_BYTES {
            return Err(EnumLabelKeyError::TooLong(digits.len() / 2).into());
        }
        let nibble = |digit: u8| match digit {
            b'0'..=b'9' => Some(digit - b'0'),
            b'a'..=b'f' => Some(digit - b'a' + 10),
            b'A'..=b'F' => Some(digit - b'A' + 10),
            _ => None,
        };
        let mut bytes = Vec::with_capacity(digits.len() / 2);
        for pair in digits.as_chunks::<2>().0 {
            let (Some(high), Some(low)) = (nibble(pair[0]), nibble(pair[1])) else {
                return Err(EnumLabelKeyParseError::Hexadecimal);
            };
            bytes.push((high << 4) | low);
        }
        Ok(Self::from_bytes(bytes)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnumLabelKeyParseError {
    #[error("enum label key is not hexadecimal text")]
    Hexadecimal,
    #[error(transparent)]
    Key(#[from] EnumLabelKeyError),
}

/// A value of one user-defined enum type. SQL binding never compares values of
/// different enum types; the derived order sorts them by type OID only so that
/// internal ordered containers remain total.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnumValue {
    type_oid: u32,
    key: EnumLabelKey,
}

impl EnumValue {
    pub fn new(type_oid: u32, key: EnumLabelKey) -> Self {
        Self { type_oid, key }
    }

    pub fn type_oid(&self) -> u32 {
        self.type_oid
    }

    pub fn key(&self) -> &EnumLabelKey {
        &self.key
    }

    /// Heap bytes owned by this value beyond its inline layout.
    pub fn retained_bytes(&self) -> usize {
        self.key.0.len()
    }
}

fn validate(bytes: &[u8]) -> Result<(), EnumLabelKeyError> {
    match bytes.last() {
        None => Err(EnumLabelKeyError::Empty),
        Some(0) => Err(EnumLabelKeyError::TrailingZero),
        Some(_) if bytes.len() > MAX_ENUM_LABEL_KEY_BYTES => {
            Err(EnumLabelKeyError::TooLong(bytes.len()))
        }
        Some(_) => Ok(()),
    }
}

/// Whether `width` base-256 digits give every slot at least two units.
fn digits_hold(width: usize, slots: usize) -> bool {
    // 256^width >= 2 * slots, evaluated without overflow.
    let mut capacity = 1_u128;
    for _ in 0..width {
        capacity = capacity.saturating_mul(256);
    }
    capacity >= 2 * slots as u128
}

/// Digits of floor(index * 256^width / slots) with trailing zeros removed.
fn scaled_fraction(index: usize, slots: usize, width: usize) -> Vec<u8> {
    // Long division of index / slots in base 256, one digit per step.
    let mut digits = Vec::with_capacity(width);
    let mut remainder = index as u128;
    let slots = slots as u128;
    for _ in 0..width {
        remainder *= 256;
        digits.push(u8::try_from(remainder / slots).expect("proper fraction digit"));
        remainder %= slots;
    }
    while digits.last() == Some(&0) {
        digits.pop();
    }
    digits
}

/// Shortest-prefix successor used for appends: increment the first digit that
/// is below 255, or extend an all-255 key by one.
fn increment(lower: &[u8]) -> Vec<u8> {
    if let Some(position) = lower.iter().position(|digit| *digit < u8::MAX) {
        let mut next = lower[..=position].to_vec();
        next[position] += 1;
        next
    } else {
        let mut next = lower.to_vec();
        next.push(1);
        next
    }
}

/// Digit-wise midpoint of `lower < upper`, where an absent upper bound is 1.0.
/// While both bounds share a digit it is copied; the first gap of two or more
/// units receives its middle digit. A gap of exactly one unit copies the lower
/// digit and continues below an open upper bound.
fn midpoint(lower: &[u8], upper: Option<&[u8]>) -> Result<Vec<u8>, EnumLabelKeyError> {
    let mut output = Vec::new();
    let mut upper = upper;
    let mut position = 0;
    loop {
        let low = u16::from(lower.get(position).copied().unwrap_or(0));
        let high = match upper {
            Some(upper) => u16::from(
                *upper
                    .get(position)
                    .ok_or(EnumLabelKeyError::UnorderedBounds)?,
            ),
            None => 256,
        };
        if high < low {
            return Err(EnumLabelKeyError::UnorderedBounds);
        }
        if high - low >= 2 {
            output.push(u8::try_from(u16::midpoint(low, high)).expect("digit midpoint"));
            return Ok(output);
        }
        output.push(u8::try_from(low).expect("lower digit"));
        if high - low == 1 {
            upper = None;
        }
        position += 1;
        if position > MAX_ENUM_LABEL_KEY_BYTES {
            return Err(EnumLabelKeyError::TooLong(position));
        }
    }
}

#[cfg(test)]
mod tests;
