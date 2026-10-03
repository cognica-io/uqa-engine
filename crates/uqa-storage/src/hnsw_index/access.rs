//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The graph algorithm borrows resident nodes or owns one decoded page at a time.

use super::{
    prepare::{check, Control},
    store::{Read, Record},
    types::{HNSWIndex, HNSWNode, NodeId},
};
use crate::StorageBackendResult;
use uqa_core::memory::MemoryError;

impl HNSWIndex {
    pub(super) fn node(&self, id: NodeId) -> StorageBackendResult<Option<Read<'_, HNSWNode>>> {
        self.nodes.get(u128::from(id))
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
