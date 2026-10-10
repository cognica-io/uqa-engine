//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private topology pages contain identities and adjacency, independently of dense vectors.

use super::{invalid, Record};
use crate::hnsw_index::types::HNSWNode;
use crate::StorageBackendResult;
use uqa_core::memory::MemoryError;

impl Record for HNSWNode {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        let mut bytes = size_of::<Self>()
            .checked_add(
                self.neighbors
                    .capacity()
                    .checked_mul(size_of::<Vec<u64>>())
                    .ok_or(MemoryError::SizeOverflow)?,
            )
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
        let mut bytes = 8_usize + 8 + 4 + 8 + 1 + 8;
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
        bytes.extend_from_slice(&(self.level as u64).to_le_bytes());
        bytes.push(u8::from(self.deleted));
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
        let level = usize::try_from(u64::from_le_bytes(take(&mut bytes)?))
            .map_err(|_| MemoryError::SizeOverflow)?;
        let deleted = match take::<1>(&mut bytes)?[0] {
            0 => false,
            1 => true,
            _ => return Err(invalid("deleted flag")),
        };
        let layers = length(&mut bytes, 8)?;
        if level > super::super::metric::MAX_HNSW_LEVEL || layers != level + 1 {
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
            level,
            deleted,
            neighbors,
        })
    }
}

pub(super) fn take<const N: usize>(bytes: &mut &[u8]) -> StorageBackendResult<[u8; N]> {
    let (value, remaining) = bytes
        .split_at_checked(N)
        .ok_or_else(|| invalid("truncated node"))?;
    *bytes = remaining;
    Ok(value.try_into().expect("checked field width"))
}

pub(super) fn length(bytes: &mut &[u8], width: usize) -> StorageBackendResult<usize> {
    let count =
        usize::try_from(u64::from_le_bytes(take(bytes)?)).map_err(|_| MemoryError::SizeOverflow)?;
    if count > bytes.len() / width {
        return Err(invalid("node array length"));
    }
    Ok(count)
}

#[test]
fn topology_pages_preserve_identities_and_reject_malformed_shapes() {
    let node = HNSWNode {
        id: u64::MAX - 1,
        doc_id: u64::MAX,
        vector_ordinal: u32::MAX,
        level: 1,
        deleted: true,
        neighbors: vec![vec![1, 7], vec![7]],
    };
    let mut bytes = Vec::new();
    node.encode(&mut bytes);
    assert_eq!(bytes.len(), node.encoded_bytes().unwrap());
    let restored = HNSWNode::decode(&bytes).unwrap();
    assert_eq!(restored.id, node.id);
    assert_eq!(restored.doc_id, node.doc_id);
    assert_eq!(restored.vector_ordinal, node.vector_ordinal);
    assert_eq!(restored.level, node.level);
    assert_eq!(restored.deleted, node.deleted);
    assert_eq!(restored.neighbors, node.neighbors);
    for end in 0..bytes.len() {
        assert!(HNSWNode::decode(&bytes[..end]).is_err());
    }
    let mut invalid = bytes.clone();
    invalid[28] = 2;
    assert!(HNSWNode::decode(&invalid).is_err());
    invalid = bytes.clone();
    invalid[29..37].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(HNSWNode::decode(&invalid).is_err());
    bytes.push(0);
    assert!(HNSWNode::decode(&bytes).is_err());
}
