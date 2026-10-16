//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Packed finite bit sequences with one canonical representation per value.

use crate::{
    memory::{Produced, ProductionControl, ProductionVec},
    ValueRetentionError,
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Most-significant-bit-first bytes and their exact logical length. Unused low bits of the last byte are zero; no excess bytes are retained. SQL type identity and declared length constraints belong to SQL's type owner.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BitStringValue {
    storage: Box<BitStringStorage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename = "BitStringValue", try_from = "SerializedBitString")]
struct BitStringStorage {
    bytes: Vec<u8>,
    bit_len: u32,
}

#[derive(Deserialize)]
struct SerializedBitString {
    bytes: Vec<u8>,
    bit_len: u32,
}

impl TryFrom<SerializedBitString> for BitStringStorage {
    type Error = &'static str;

    fn try_from(value: SerializedBitString) -> Result<Self, Self::Error> {
        if !valid_packed(&value.bytes, value.bit_len) {
            return Err("invalid packed bit-string length or padding");
        }
        Ok(Self {
            bytes: value.bytes,
            bit_len: value.bit_len,
        })
    }
}

impl BitStringValue {
    /// Validate an owned packed representation without normalizing or copying its bytes.
    pub fn try_from_packed(bytes: Vec<u8>, bit_len: u32) -> Option<Self> {
        if !valid_packed(&bytes, bit_len) {
            return None;
        }
        Some(Self::from_validated_parts(bytes, bit_len))
    }

    /// Transfer already-admitted packed bytes without copying; malformed representations release their original reservation.
    pub fn try_from_packed_with_control(
        bytes: Produced<Vec<u8>>,
        bit_len: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        control.check()?;
        if !valid_packed(&bytes, bit_len) {
            return Ok(None);
        }
        let header = control.reserve(Self::retained_header_bytes())?;
        let (bytes, memory) = bytes.into_parts();
        control
            .finish(
                Self::from_validated_parts(bytes, bit_len),
                control.combine(memory, header),
            )
            .map(Some)
    }

    fn from_validated_parts(bytes: Vec<u8>, bit_len: u32) -> Self {
        Self {
            storage: Box::new(BitStringStorage { bytes, bit_len }),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.storage.bytes
    }

    /// Reserve this boxed header before transferring separately admitted packed bytes.
    pub const fn retained_header_bytes() -> usize {
        size_of::<BitStringStorage>()
    }

    /// Heap bytes owned by the packed payload and its indirect header.
    pub fn retained_bytes(&self) -> usize {
        Self::retained_header_bytes() + self.storage.bytes.capacity()
    }

    pub fn bit_len(&self) -> u32 {
        self.storage.bit_len
    }

    pub fn is_empty(&self) -> bool {
        self.bit_len() == 0
    }

    pub fn bit(&self, position: u32) -> Option<bool> {
        (position < self.bit_len())
            .then(|| self.as_bytes()[position as usize / 8] & (0x80 >> (position % 8)) != 0)
    }

    /// Copy packed bytes under the original allowance, checking both cancellation owners between bounded chunks.
    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        control.check()?;
        let header = control.reserve(Self::retained_header_bytes())?;
        let mut bytes = ProductionVec::new(*control);
        bytes.reserve(self.as_bytes().len())?;
        for chunk in self.as_bytes().chunks(4096) {
            control.check()?;
            for byte in chunk {
                bytes.push_copy(*byte)?;
            }
        }
        let (bytes, memory) = bytes.finish()?.into_parts();
        control.finish(
            Self::from_validated_parts(bytes, self.bit_len()),
            control.combine(memory, header),
        )
    }

    /// Compare logical bit sequences lexicographically without unpacking or allocating scratch.
    pub fn cmp_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering, ValueRetentionError> {
        control.check()?;
        for (left, right) in self
            .as_bytes()
            .chunks(4096)
            .zip(other.as_bytes().chunks(4096))
        {
            control.check()?;
            let order = left.cmp(right);
            if !order.is_eq() {
                return Ok(order);
            }
        }
        Ok(self.bit_len().cmp(&other.bit_len()))
    }
}

fn valid_packed(bytes: &[u8], bit_len: u32) -> bool {
    bytes.len() == (u64::from(bit_len).div_ceil(8) as usize)
        && (bit_len.is_multiple_of(8)
            || bytes
                .last()
                .is_some_and(|last| last & ((1_u8 << (8 - bit_len % 8)) - 1) == 0))
}

impl Ord for BitStringValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_bytes()
            .cmp(other.as_bytes())
            .then_with(|| self.bit_len().cmp(&other.bit_len()))
    }
}

impl PartialOrd for BitStringValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests;
