//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One output continuation preserves node order, exact vector bits and original adjacency order.

use super::{
    invalid, validation::Validation, HNSWIndex, ImageRead, StorageBackendResult, StorageReadControl,
};
use crate::{
    hnsw_index::store::{NodeRead, VectorRead},
    image_io::ImagePreparation,
    vector_index::image::{coordinate_bytes, MAGIC},
};
use uqa_core::memory::MemoryError;

enum Stage {
    Node,
    Raw,
    RawValues,
    Normalized,
    NormalizedValues,
    Layer,
    Neighbors,
    Dirty,
    Complete,
}

pub(super) struct HNSWImageCursor<'a> {
    index: &'a HNSWIndex,
    validation: Validation<'a>,
    stage: Stage,
    after: Option<u128>,
    node: Option<NodeRead<'a>>,
    vector: Option<VectorRead<'a>>,
    layer: usize,
    offset: usize,
    pending: [u8; 4096],
    length: usize,
    position: usize,
    control: &'a StorageReadControl,
}

impl<'a> HNSWImageCursor<'a> {
    pub(super) fn new(
        index: &'a HNSWIndex,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut cursor = Self {
            index,
            validation: Validation::new(index),
            stage: Stage::Node,
            after: None,
            node: None,
            vector: None,
            layer: 0,
            offset: 0,
            pending: [0; 4096],
            length: 0,
            position: 0,
            control,
        };
        cursor.bytes(MAGIC);
        cursor.bytes(&[2]);
        cursor.bytes(&index.dimensions.to_le_bytes());
        for value in [
            index.params.m,
            index.params.ef_construction,
            index.params.ef_search,
            index.params.rebuild_threshold,
        ] {
            cursor.count(value)?;
        }
        cursor.bytes(&index.params.seed.to_le_bytes());
        cursor.bytes(&[u8::from(index.entry_point.is_some())]);
        if let Some(entry) = index.entry_point {
            cursor.bytes(&entry.to_le_bytes());
        }
        cursor.count(index.max_level)?;
        cursor.bytes(&index.next_node_id.to_le_bytes());
        cursor.count(index.deleted_count)?;
        cursor.bytes(&[u8::from(index.full_rewrite)]);
        cursor.count(index.nodes.len())?;
        Ok(cursor)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.pending[self.length..self.length + bytes.len()].copy_from_slice(bytes);
        self.length += bytes.len();
    }

    fn count(&mut self, value: usize) -> StorageBackendResult<()> {
        self.bytes(
            &u64::try_from(value)
                .map_err(|_| MemoryError::SizeOverflow)?
                .to_le_bytes(),
        );
        Ok(())
    }

    fn next_piece(&mut self) -> StorageBackendResult<bool> {
        self.position = 0;
        self.length = 0;
        loop {
            self.control.check()?;
            match self.stage {
                Stage::Node => {
                    if let Some(id) = self
                        .index
                        .nodes
                        .next_key(self.after, self.control.memory())?
                    {
                        let node = NodeRead::open(&self.index.nodes, id, self.control)?
                            .ok_or_else(invalid)?;
                        self.bytes(&node.id.to_le_bytes());
                        self.bytes(&node.doc_id.to_le_bytes());
                        self.bytes(&node.vector_ordinal.to_le_bytes());
                        self.count(node.level)?;
                        self.bytes(&[u8::from(node.deleted)]);
                        self.after = Some(id);
                        self.node = Some(node);
                        self.stage = Stage::Raw;
                    } else {
                        self.count(self.index.dirty_nodes.len())?;
                        self.after = None;
                        self.stage = Stage::Dirty;
                    }
                }
                Stage::Raw | Stage::Normalized => self.start_vector()?,
                Stage::RawValues | Stage::NormalizedValues => {
                    let vector = self.vector.as_ref().expect("selected output vector");
                    if self.offset == vector.len() {
                        self.vector = None;
                        if matches!(self.stage, Stage::RawValues) {
                            self.stage = Stage::Normalized;
                        } else {
                            self.layer = 0;
                            self.stage = Stage::Layer;
                        }
                        continue;
                    }
                    let mut values = [0_f32; 1024];
                    let count = vector.read_values(self.offset, &mut values, self.control)?;
                    self.length = coordinate_bytes(&mut values[..count].iter(), &mut self.pending)?;
                    self.offset += count;
                }
                Stage::Layer => {
                    let node = self.node.as_ref().expect("selected output node");
                    if self.layer == node.layer_count() {
                        self.node = None;
                        self.stage = Stage::Node;
                        continue;
                    }
                    self.count(node.neighbor_count(self.layer)?)?;
                    self.offset = 0;
                    self.stage = Stage::Neighbors;
                }
                Stage::Neighbors => {
                    let node = self.node.as_ref().expect("selected output node");
                    if self.offset == node.neighbor_count(self.layer)? {
                        self.layer += 1;
                        self.stage = Stage::Layer;
                        continue;
                    }
                    let mut values = [0_u64; 512];
                    let count =
                        node.read_neighbors(self.layer, self.offset, &mut values, self.control)?;
                    for (value, bytes) in values[..count]
                        .iter()
                        .zip(self.pending[..count * 8].as_chunks_mut::<8>().0)
                    {
                        *bytes = value.to_le_bytes();
                    }
                    self.length = count * 8;
                    self.offset += count;
                }
                Stage::Dirty => {
                    let Some(key) = self
                        .index
                        .dirty_nodes
                        .next_key(self.after, self.control.memory())?
                    else {
                        self.stage = Stage::Complete;
                        return Ok(false);
                    };
                    let value = self
                        .index
                        .dirty_nodes
                        .get_with_memory(key, self.control.memory())?
                        .ok_or_else(invalid)?;
                    self.bytes(&u64::try_from(key).map_err(|_| invalid())?.to_le_bytes());
                    self.bytes(&value.to_le_bytes());
                    self.after = Some(key);
                }
                Stage::Complete => return Ok(false),
            }
            return Ok(true);
        }
    }
    fn start_vector(&mut self) -> StorageBackendResult<()> {
        let normalized = matches!(self.stage, Stage::Normalized);
        let map = if normalized {
            &self.index.normalized_vectors
        } else {
            &self.index.raw_vectors
        };
        let vector =
            VectorRead::open(map, self.after.expect("selected output node"), self.control)?
                .ok_or_else(invalid)?;
        self.bytes(&vector.norm().to_bits().to_le_bytes());
        self.count(vector.len())?;
        self.vector = Some(vector);
        self.offset = 0;
        self.stage = if normalized {
            Stage::NormalizedValues
        } else {
            Stage::RawValues
        };

        Ok(())
    }
}

impl ImageRead for HNSWImageCursor<'_> {
    fn prepare(&mut self, maximum: usize) -> StorageBackendResult<ImagePreparation> {
        self.validation.advance(maximum, self.control)
    }

    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.control.check()?;
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit {
            if self.position == self.length && !self.next_piece()? {
                break;
            }
            let count = (self.length - self.position).min(limit - written);
            output[written..written + count]
                .copy_from_slice(&self.pending[self.position..self.position + count]);
            written += count;
            self.position += count;
        }
        self.control.check()?;
        Ok(written)
    }
}
