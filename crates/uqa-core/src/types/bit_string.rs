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
#[serde(try_from = "SerializedBitString")]
pub struct BitStringValue {
    bytes: Vec<u8>,
    bit_len: u32,
}

#[derive(Deserialize)]
struct SerializedBitString {
    bytes: Vec<u8>,
    bit_len: u32,
}

impl TryFrom<SerializedBitString> for BitStringValue {
    type Error = &'static str;

    fn try_from(value: SerializedBitString) -> Result<Self, Self::Error> {
        Self::try_from_packed(value.bytes, value.bit_len)
            .ok_or("invalid packed bit-string length or padding")
    }
}

impl BitStringValue {
    /// Validate an owned packed representation without normalizing or copying its bytes.
    pub fn try_from_packed(bytes: Vec<u8>, bit_len: u32) -> Option<Self> {
        if !valid_packed(&bytes, bit_len) {
            return None;
        }
        Some(Self { bytes, bit_len })
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
        let (bytes, memory) = bytes.into_parts();
        control.finish(Self { bytes, bit_len }, memory).map(Some)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Heap bytes owned by the canonical packed payload, excluding its inline header.
    pub fn retained_bytes(&self) -> usize {
        self.bytes.capacity()
    }

    pub fn bit_len(&self) -> u32 {
        self.bit_len
    }

    pub fn is_empty(&self) -> bool {
        self.bit_len == 0
    }

    pub fn bit(&self, position: u32) -> Option<bool> {
        (position < self.bit_len)
            .then(|| self.bytes[position as usize / 8] & (0x80 >> (position % 8)) != 0)
    }

    /// Copy packed bytes under the original allowance, checking both cancellation owners between bounded chunks.
    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        control.check()?;
        let mut bytes = ProductionVec::new(*control);
        bytes.reserve(self.bytes.len())?;
        for chunk in self.bytes.chunks(4096) {
            control.check()?;
            for byte in chunk {
                bytes.push_copy(*byte)?;
            }
        }
        let (bytes, memory) = bytes.finish()?.into_parts();
        control.finish(
            Self {
                bytes,
                bit_len: self.bit_len,
            },
            memory,
        )
    }

    /// Compare logical bit sequences lexicographically without unpacking or allocating scratch.
    pub fn cmp_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering, ValueRetentionError> {
        control.check()?;
        for (left, right) in self.bytes.chunks(4096).zip(other.bytes.chunks(4096)) {
            control.check()?;
            let order = left.cmp(right);
            if !order.is_eq() {
                return Ok(order);
            }
        }
        Ok(self.bit_len.cmp(&other.bit_len))
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
        self.bytes
            .cmp(&other.bytes)
            .then_with(|| self.bit_len.cmp(&other.bit_len))
    }
}

impl PartialOrd for BitStringValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests;
