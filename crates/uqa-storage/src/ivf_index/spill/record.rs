//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporary IVF records preserve raw and normalized floating-point bits.

use super::{corrupt, StoredVector};
use crate::{spill_map::Record, StorageBackendResult};
use uqa_core::memory::MemoryError;

impl Record for StoredVector {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        self.raw_vector
            .capacity()
            .checked_add(self.vector.capacity())
            .and_then(|count| count.checked_mul(4))
            .and_then(|bytes| bytes.checked_add(size_of::<Self>()))
            .ok_or_else(|| MemoryError::SizeOverflow.into())
    }
    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        self.raw_vector
            .len()
            .checked_add(self.vector.len())
            .and_then(|count| count.checked_mul(4))
            .and_then(|bytes| bytes.checked_add(41))
            .ok_or_else(|| MemoryError::SizeOverflow.into())
    }
    fn encode(&self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(&self.doc_id.to_le_bytes());
        bytes.extend_from_slice(&self.vector_ordinal.to_le_bytes());
        bytes.extend_from_slice(&self.norm.to_bits().to_le_bytes());
        bytes.push(u8::from(self.centroid.is_some()));
        bytes.extend_from_slice(&(self.centroid.unwrap_or(0) as u64).to_le_bytes());
        for vector in [&self.raw_vector, &self.vector] {
            bytes.extend_from_slice(&(vector.len() as u64).to_le_bytes());
            for value in vector {
                bytes.extend_from_slice(&value.to_bits().to_le_bytes());
            }
        }
    }
    fn decode(mut bytes: &[u8]) -> StorageBackendResult<Self> {
        let doc_id = u64::from_le_bytes(take(&mut bytes)?);
        let vector_ordinal = u32::from_le_bytes(take(&mut bytes)?);
        let norm = f32::from_bits(u32::from_le_bytes(take(&mut bytes)?));
        let present = take::<1>(&mut bytes)?[0];
        let value = usize::try_from(u64::from_le_bytes(take(&mut bytes)?))
            .map_err(|_| MemoryError::SizeOverflow)?;
        let centroid = match present {
            0 => None,
            1 => Some(value),
            _ => return Err(corrupt("temporary centroid flag")),
        };
        let raw_vector = floats(&mut bytes)?;
        let vector = floats(&mut bytes)?;
        if !bytes.is_empty() || raw_vector.len() != vector.len() {
            return Err(corrupt("temporary vector shape"));
        }
        Ok(Self {
            key: (doc_id, vector_ordinal),
            doc_id,
            vector_ordinal,
            raw_vector,
            norm,
            vector,
            centroid,
        })
    }
}

fn take<const N: usize>(bytes: &mut &[u8]) -> StorageBackendResult<[u8; N]> {
    let (head, tail) = bytes
        .split_at_checked(N)
        .ok_or_else(|| corrupt("truncated temporary vector"))?;
    *bytes = tail;
    Ok(head.try_into().expect("checked field width"))
}
fn floats(bytes: &mut &[u8]) -> StorageBackendResult<Vec<f32>> {
    let count =
        usize::try_from(u64::from_le_bytes(take(bytes)?)).map_err(|_| MemoryError::SizeOverflow)?;
    if count > bytes.len() / 4 {
        return Err(corrupt("temporary vector length"));
    }
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        result.push(f32::from_bits(u32::from_le_bytes(take(bytes)?)));
    }
    Ok(result)
}

#[cfg(test)]
#[test]
fn temporary_vectors_preserve_coordinate_bits_and_reject_truncated_lengths() {
    let bits = [0, 0x8000_0000, 1, 0x7f7f_ffff, 0xff7f_ffff];
    let source = StoredVector {
        key: (u64::MAX, u32::MAX),
        doc_id: u64::MAX,
        vector_ordinal: u32::MAX,
        raw_vector: bits.map(f32::from_bits).to_vec(),
        norm: f32::INFINITY,
        vector: bits.map(f32::from_bits).to_vec(),
        centroid: Some(usize::MAX),
    };
    let mut bytes = Vec::new();
    source.encode(&mut bytes);
    assert_eq!(bytes.len(), source.encoded_bytes().unwrap());
    let decoded = StoredVector::decode(&bytes).unwrap();
    assert_eq!(decoded.key, source.key);
    assert_eq!(decoded.centroid, source.centroid);
    assert_eq!(decoded.norm.to_bits(), source.norm.to_bits());
    for vector in [&decoded.raw_vector, &decoded.vector] {
        assert_eq!(
            vector
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            bits
        );
    }
    for end in [0, 12, 40, bytes.len() - 1] {
        assert!(StoredVector::decode(&bytes[..end]).is_err());
    }
    bytes[25..33].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(StoredVector::decode(&bytes).is_err());
}
