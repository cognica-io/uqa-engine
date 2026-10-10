//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The graph algorithm borrows resident nodes or owns one decoded page at a time.

use super::{
    prepare::{check, Control},
    store::{Read, Record},
    types::{HNSWIndex, HNSWNode, HNSWVector, NodeId},
};
use crate::StorageBackendResult;
use uqa_core::memory::MemoryError;

impl HNSWIndex {
    pub(super) fn node(&self, id: NodeId) -> StorageBackendResult<Option<Read<'_, HNSWNode>>> {
        self.nodes.get(u128::from(id))
    }

    pub(super) fn raw_vector(&self, id: NodeId) -> StorageBackendResult<Read<'_, HNSWVector>> {
        self.raw_vectors.get(u128::from(id))?.ok_or_else(|| {
            crate::StorageBackendError::Other(format!("HNSW node {id} has no canonical vector"))
        })
    }

    pub(super) fn normalized_vector(
        &self,
        id: NodeId,
    ) -> StorageBackendResult<Read<'_, HNSWVector>> {
        self.normalized_vectors.get(u128::from(id))?.ok_or_else(|| {
            crate::StorageBackendError::Other(format!("HNSW node {id} has no normalized vector"))
        })
    }

    pub(super) fn put_vectors(
        &mut self,
        id: NodeId,
        raw: Vec<f32>,
        normalized: Vec<f32>,
        norm: f32,
        control: Control<'_>,
    ) -> StorageBackendResult<()> {
        self.raw_vectors
            .insert(u128::from(id), HNSWVector { values: raw, norm }, control)?;
        self.normalized_vectors.insert(
            u128::from(id),
            HNSWVector {
                values: normalized,
                norm: 1.0,
            },
            control,
        )
    }

    pub(super) fn put_node(
        &mut self,
        node: HNSWNode,
        control: Control<'_>,
    ) -> StorageBackendResult<()> {
        let id = u128::from(node.id);
        self.nodes.insert(id, node, control)?;
        self.dirty_nodes.insert(id, 0, control)
    }

    pub(super) fn modify_node(
        &mut self,
        id: NodeId,
        control: Control<'_>,
        change: impl FnOnce(&mut HNSWNode),
    ) -> StorageBackendResult<()> {
        check(control)?;
        let Some(node) = self.node(id)? else {
            return Ok(());
        };
        let bytes = node
            .memory_bytes()?
            .checked_add(
                self.params
                    .m
                    .checked_mul(4 * size_of::<NodeId>())
                    .ok_or(MemoryError::SizeOverflow)?,
            )
            .ok_or(MemoryError::SizeOverflow)?;
        let _copy = self.memory.reserve(bytes)?;
        let mut owned = (*node).clone();
        drop(node);
        change(&mut owned);
        self.put_node(owned, control)
    }
}
