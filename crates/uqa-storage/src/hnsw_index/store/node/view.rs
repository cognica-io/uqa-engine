//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed topology records expose fixed adjacency windows without decoding complete nodes.

use super::take;
use crate::{
    hnsw_index::{types::HNSWNode, MAX_HNSW_LEVEL},
    read_control::StorageReadControl,
    spill_map::{invalid, Map, RecordBytes, RecordRead},
    StorageBackendResult,
};
use uqa_core::memory::MemoryError;

#[cfg(test)]
mod tests;

enum Source<'a> {
    Resident(&'a HNSWNode),
    Spilled(RecordBytes<'a>),
}

#[derive(Clone, Copy, Default)]
struct Layer {
    offset: usize,
    length: usize,
}

pub(in crate::hnsw_index) struct NodeRead<'a> {
    pub(in crate::hnsw_index) id: u64,
    pub(in crate::hnsw_index) doc_id: u64,
    pub(in crate::hnsw_index) vector_ordinal: u32,
    pub(in crate::hnsw_index) level: usize,
    pub(in crate::hnsw_index) deleted: bool,
    source: Source<'a>,
    layers: [Layer; MAX_HNSW_LEVEL + 1],
    layer_count: usize,
}

impl<'a> NodeRead<'a> {
    pub(in crate::hnsw_index) fn open(
        map: &'a Map<HNSWNode>,
        key: u128,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<Self>> {
        let Some(record) = map.read_record(key, control)? else {
            return Ok(None);
        };
        let view = match record {
            RecordRead::Resident(node) => Self {
                id: node.id,
                doc_id: node.doc_id,
                vector_ordinal: node.vector_ordinal,
                level: node.level,
                deleted: node.deleted,
                layer_count: node.neighbors.len(),
                source: Source::Resident(node),
                layers: [Layer::default(); MAX_HNSW_LEVEL + 1],
            },
            RecordRead::Spilled(record) => Self::spilled(record, control)?,
        };
        control.check()?;
        Ok(Some(view))
    }

    fn spilled(
        record: RecordBytes<'a>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        if record.len() < 37 {
            return Err(invalid("truncated node"));
        }
        let header = record.array::<37>(0, control)?;
        let mut bytes = header.as_slice();
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
        let layer_count = usize::try_from(u64::from_le_bytes(take(&mut bytes)?))
            .map_err(|_| MemoryError::SizeOverflow)?;
        if layer_count > (record.len() - 37) / 8 {
            return Err(invalid("node array length"));
        }
        if level > MAX_HNSW_LEVEL || layer_count != level + 1 {
            return Err(invalid("node shape"));
        }
        let mut layers = [Layer::default(); MAX_HNSW_LEVEL + 1];
        let mut offset = 37;
        for layer in &mut layers[..layer_count] {
            if record.len() - offset < 8 {
                return Err(invalid("truncated node"));
            }
            let length = usize::try_from(u64::from_le_bytes(record.array(offset, control)?))
                .map_err(|_| MemoryError::SizeOverflow)?;
            offset += 8;
            if length > (record.len() - offset) / 8 {
                return Err(invalid("node array length"));
            }
            *layer = Layer { offset, length };
            offset += length * 8;
        }
        if offset != record.len() {
            return Err(invalid("trailing node bytes"));
        }
        Ok(Self {
            id,
            doc_id,
            vector_ordinal,
            level,
            deleted,
            source: Source::Spilled(record),
            layers,
            layer_count,
        })
    }

    pub(in crate::hnsw_index) fn layer_count(&self) -> usize {
        self.layer_count
    }

    pub(in crate::hnsw_index) fn neighbor_count(
        &self,
        layer: usize,
    ) -> StorageBackendResult<usize> {
        if layer >= self.layer_count {
            return Err(invalid("node layer"));
        }
        Ok(match &self.source {
            Source::Resident(node) => node.neighbors[layer].len(),
            Source::Spilled(_) => self.layers[layer].length,
        })
    }

    /// Copy at most 512 adjacency identities into the caller's fixed window.
    pub(in crate::hnsw_index) fn read_neighbors(
        &self,
        layer: usize,
        offset: usize,
        output: &mut [u64],
        control: &StorageReadControl,
    ) -> StorageBackendResult<usize> {
        control.check()?;
        let remaining = self
            .neighbor_count(layer)?
            .checked_sub(offset)
            .ok_or_else(|| invalid("adjacency read offset"))?;
        let count = remaining.min(output.len()).min(512);
        match &self.source {
            Source::Resident(node) => {
                output[..count].copy_from_slice(&node.neighbors[layer][offset..offset + count]);
            }
            Source::Spilled(record) => {
                let mut bytes = [0; 4096];
                let start = offset
                    .checked_mul(8)
                    .and_then(|offset| self.layers[layer].offset.checked_add(offset))
                    .ok_or(MemoryError::SizeOverflow)?;
                if record.read_at(start, &mut bytes[..count * 8], control)? != count * 8 {
                    return Err(invalid("truncated node adjacency"));
                }
                for (value, bytes) in output[..count]
                    .iter_mut()
                    .zip(bytes[..count * 8].as_chunks::<8>().0)
                {
                    *value = u64::from_le_bytes(*bytes);
                }
            }
        }
        control.check()?;
        Ok(count)
    }
}
