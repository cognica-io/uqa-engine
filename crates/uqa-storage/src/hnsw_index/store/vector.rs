//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical and normalized vectors spill independently; traversal reads only its distance input.

use super::{
    invalid,
    node::{length, take},
    Record,
};
use crate::{hnsw_index::types::HNSWVector, StorageBackendResult};
use uqa_core::memory::MemoryError;

impl Record for HNSWVector {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        Ok(size_of::<Self>()
            .checked_add(
                self.values
                    .capacity()
                    .checked_mul(4)
                    .ok_or(MemoryError::SizeOverflow)?,
            )
            .ok_or(MemoryError::SizeOverflow)?)
    }

    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        Ok(12_usize
            .checked_add(
                self.values
                    .len()
                    .checked_mul(4)
                    .ok_or(MemoryError::SizeOverflow)?,
            )
            .ok_or(MemoryError::SizeOverflow)?)
    }

    fn encode(&self, bytes: &mut Vec<u8>) {
        #[cfg(test)]
        super::ENCODED_VECTORS.set(super::ENCODED_VECTORS.get() + 1);
        bytes.extend_from_slice(&self.norm.to_bits().to_le_bytes());
        bytes.extend_from_slice(&(self.values.len() as u64).to_le_bytes());
        for value in &self.values {
            bytes.extend_from_slice(&value.to_bits().to_le_bytes());
        }
    }

    fn decode(mut bytes: &[u8]) -> StorageBackendResult<Self> {
        let norm = f32::from_bits(u32::from_le_bytes(take(&mut bytes)?));
        let count = length(&mut bytes, 4)?;
        #[cfg(test)]
        super::DECODED_VECTOR_FLOATS.set(super::DECODED_VECTOR_FLOATS.get() + count);
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(f32::from_bits(u32::from_le_bytes(take(&mut bytes)?)));
        }
        if !bytes.is_empty() {
            return Err(invalid("trailing vector bytes"));
        }
        Ok(Self { values, norm })
    }
}

#[cfg(test)]
#[test]
fn vector_pages_preserve_float_bits_and_reject_malformed_lengths() {
    let bits = [0, 0x8000_0000, 0x7f7f_ffff, 0xff7f_ffff, 0x7fc0_0001];
    let vector = HNSWVector {
        values: bits.map(f32::from_bits).to_vec(),
        norm: f32::from_bits(0x7fc0_0002),
    };
    let mut bytes = Vec::new();
    vector.encode(&mut bytes);
    assert_eq!(bytes.len(), vector.encoded_bytes().unwrap());
    let decoded = HNSWVector::decode(&bytes).unwrap();
    assert_eq!(
        decoded
            .values
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        bits
    );
    assert_eq!(decoded.norm.to_bits(), vector.norm.to_bits());
    for length in [0, 8, bytes.len() - 1] {
        assert!(HNSWVector::decode(&bytes[..length]).is_err());
    }
    bytes.push(0);
    assert!(HNSWVector::decode(&bytes).is_err());
}
