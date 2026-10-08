//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private graph pages preserve every floating-point bit and adjacency entry.

use super::{invalid, Record};
use crate::hnsw_index::types::HNSWNode;
use crate::StorageBackendResult;
use uqa_core::memory::MemoryError;

impl Record for HNSWNode {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        let mut bytes = size_of::<Self>()
            .checked_add(
                self.raw_vector
                    .capacity()
                    .checked_mul(4)
                    .ok_or(MemoryError::SizeOverflow)?,
            )
            .and_then(|n| n.checked_add(self.normalized_vector.capacity().checked_mul(4)?))
            .and_then(|n| {
                n.checked_add(
                    self.neighbors
                        .capacity()
                        .checked_mul(size_of::<Vec<u64>>())?,
                )
            })
            .ok_or(MemoryError::SizeOverflow)?;
        for layer in &self.neighbors {
            bytes = bytes
                .checked_add(
                    layer
                        .capacity()
                        .checked_mul(8)
                        .ok_or(MemoryError::SizeOverflow)?,
                )
                .ok_or(MemoryError::SizeOverflow)?;
        }
        Ok(bytes)
    }

    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        let mut bytes = 8_usize + 8 + 4 + 4 + 8 + 1 + 8 + 8 + 8;
        bytes = bytes
            .checked_add(
                self.raw_vector
                    .len()
                    .checked_mul(4)
                    .ok_or(MemoryError::SizeOverflow)?,
            )
            .and_then(|n| n.checked_add(self.normalized_vector.len().checked_mul(4)?))
            .ok_or(MemoryError::SizeOverflow)?;
        for layer in &self.neighbors {
            bytes = bytes
                .checked_add(8)
                .and_then(|n| n.checked_add(layer.len().checked_mul(8)?))
                .ok_or(MemoryError::SizeOverflow)?;
        }
        Ok(bytes)
    }

    fn encode(&self, bytes: &mut Vec<u8>) {
        #[cfg(test)]
        super::ENCODED_NODES.set(super::ENCODED_NODES.get() + 1);
        bytes.extend_from_slice(&self.id.to_le_bytes());
        bytes.extend_from_slice(&self.doc_id.to_le_bytes());
        bytes.extend_from_slice(&self.vector_ordinal.to_le_bytes());
        bytes.extend_from_slice(&self.norm.to_bits().to_le_bytes());
        bytes.extend_from_slice(&(self.level as u64).to_le_bytes());
        bytes.push(u8::from(self.deleted));
        for vector in [&self.raw_vector, &self.normalized_vector] {
            bytes.extend_from_slice(&(vector.len() as u64).to_le_bytes());
            for value in vector {
                bytes.extend_from_slice(&value.to_bits().to_le_bytes());
            }
        }
        bytes.extend_from_slice(&(self.neighbors.len() as u64).to_le_bytes());
        for layer in &self.neighbors {
            bytes.extend_from_slice(&(layer.len() as u64).to_le_bytes());
            for value in layer {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
    }

    fn decode(mut bytes: &[u8]) -> StorageBackendResult<Self> {
        let id = u64::from_le_bytes(take(&mut bytes)?);
        let doc_id = u64::from_le_bytes(take(&mut bytes)?);
        let vector_ordinal = u32::from_le_bytes(take(&mut bytes)?);
        let norm = f32::from_bits(u32::from_le_bytes(take(&mut bytes)?));
        let level = usize::try_from(u64::from_le_bytes(take(&mut bytes)?))
            .map_err(|_| MemoryError::SizeOverflow)?;
        let deleted = match take::<1>(&mut bytes)?[0] {
            0 => false,
            1 => true,
            _ => return Err(invalid("deleted flag")),
        };
        let raw_vector = floats(&mut bytes)?;
        let normalized_vector = floats(&mut bytes)?;
        let layers = length(&mut bytes, 8)?;
        if level > super::super::metric::MAX_HNSW_LEVEL
            || layers != level + 1
            || raw_vector.len() != normalized_vector.len()
        {
            return Err(invalid("node shape"));
        }
        let mut neighbors = Vec::with_capacity(layers);
        for _ in 0..layers {
            let count = length(&mut bytes, 8)?;
            let mut layer = Vec::with_capacity(count);
            for _ in 0..count {
                layer.push(u64::from_le_bytes(take(&mut bytes)?));
            }
            neighbors.push(layer);
        }
        if !bytes.is_empty() {
            return Err(invalid("trailing node bytes"));
        }
        Ok(Self {
            id,
            doc_id,
            vector_ordinal,
            raw_vector,
            norm,
            normalized_vector,
            level,
            deleted,
            neighbors,
        })
    }
}

fn take<const N: usize>(bytes: &mut &[u8]) -> StorageBackendResult<[u8; N]> {
    let (value, remaining) = bytes
        .split_at_checked(N)
        .ok_or_else(|| invalid("truncated node"))?;
    *bytes = remaining;
    Ok(value.try_into().expect("checked field width"))
}

fn length(bytes: &mut &[u8], width: usize) -> StorageBackendResult<usize> {
    let count =
        usize::try_from(u64::from_le_bytes(take(bytes)?)).map_err(|_| MemoryError::SizeOverflow)?;
    if count > bytes.len() / width {
        return Err(invalid("node array length"));
    }
    Ok(count)
}

fn floats(bytes: &mut &[u8]) -> StorageBackendResult<Vec<f32>> {
    let count = length(bytes, 4)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(f32::from_bits(u32::from_le_bytes(take(bytes)?)));
    }
    Ok(values)
}

#[cfg(test)]
#[test]
fn node_pages_keep_float_bits_and_reject_truncated_arrays() {
    let bits = [0, 0x8000_0000, 0x7f7f_ffff, 0xff7f_ffff, 0x7fc0_0001];
    let node = crate::hnsw_index::types::HNSWNode {
        id: u64::MAX,
        doc_id: 8,
        vector_ordinal: u32::MAX,
        raw_vector: bits.map(f32::from_bits).to_vec(),
        normalized_vector: bits.map(f32::from_bits).to_vec(),
        norm: f32::from_bits(0x7fc0_0002),
        level: 1,
        deleted: true,
        neighbors: vec![vec![0, 2, u64::MAX], vec![u64::MAX - 1]],
    };
    let mut bytes = Vec::new();
    node.encode(&mut bytes);
    assert_eq!(bytes.len(), node.encoded_bytes().unwrap());
    let decoded = crate::hnsw_index::types::HNSWNode::decode(&bytes).unwrap();
    assert_eq!(
        decoded
            .raw_vector
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        bits
    );
    assert_eq!(
        decoded
            .normalized_vector
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        bits
    );
    assert_eq!(decoded.norm.to_bits(), node.norm.to_bits());
    assert_eq!(decoded.neighbors, node.neighbors);
    assert_eq!(
        (
            decoded.id,
            decoded.doc_id,
            decoded.vector_ordinal,
            decoded.level,
            decoded.deleted
        ),
        (
            node.id,
            node.doc_id,
            node.vector_ordinal,
            node.level,
            node.deleted
        )
    );
    for length in [0, 8, bytes.len() - 1] {
        assert!(crate::hnsw_index::types::HNSWNode::decode(&bytes[..length]).is_err());
    }
}
