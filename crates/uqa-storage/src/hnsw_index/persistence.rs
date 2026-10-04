//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistence deltas retain immutable roots and decode one changed node at a time.

use super::types::{HNSWGraphMeta, HNSWIndex, HNSWNode, HNSWNodeSnapshot, HNSWPersistenceDelta};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryError};

#[derive(Debug, Clone)]
pub struct HNSWGraphDelta {
    pub meta: HNSWGraphMeta,
    pub full_rewrite: bool,
    graph: HNSWIndex,
    control: StorageReadControl,
}

impl HNSWGraphDelta {
    pub fn nodes(&self) -> HNSWDeltaNodes<'_> {
        HNSWDeltaNodes {
            delta: self,
            after: None,
            done: false,
        }
    }

    /// Explicitly materialize a delta when the caller needs an owned vector. Providers consume `nodes()` instead.
    pub fn collect(&self) -> StorageBackendResult<Budgeted<HNSWPersistenceDelta>> {
        let mut nodes = BudgetedVec::new(self.control.memory());
        let mut payload = self.control.memory().empty_reservation();
        for node in self.nodes() {
            let (node, memory) = node?.into_parts();
            payload.absorb(memory);
            nodes.push(node)?;
        }
        let (nodes, memory) = nodes.into_parts();
        payload.absorb(memory);
        Ok(Budgeted::new(
            HNSWPersistenceDelta {
                meta: self.meta,
                full_rewrite: self.full_rewrite,
                nodes,
            },
            payload,
        ))
    }
}

pub struct HNSWDeltaNodes<'a> {
    delta: &'a HNSWGraphDelta,
    after: Option<u128>,
    done: bool,
}

impl Iterator for HNSWDeltaNodes<'_> {
    type Item = StorageBackendResult<Budgeted<HNSWNodeSnapshot>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let result = (|| {
            self.delta.control.check()?;
            let graph = &self.delta.graph;
            let next = if self.delta.full_rewrite {
                graph.nodes.next(self.after)?
            } else {
                match graph.dirty_nodes.next(self.after)? {
                    Some((id, _)) => Some((
                        id,
                        graph.node(id as u64)?.ok_or_else(|| {
                            crate::StorageBackendError::Other(
                                "HNSW delta references a missing node".into(),
                            )
                        })?,
                    )),
                    None => None,
                }
            };
            let Some((id, node)) = next else {
                return Ok(None);
            };
            let memory = self
                .delta
                .control
                .memory()
                .reserve(snapshot_bytes(&node)?)?;
            let snapshot = HNSWNodeSnapshot::from(&*node);
            self.after = Some(id);
            Ok(Some(Budgeted::new(snapshot, memory)))
        })();
        match result {
            Ok(Some(node)) => Some(Ok(node)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}

fn snapshot_bytes(node: &HNSWNode) -> Result<usize, MemoryError> {
    let mut bytes = size_of::<HNSWNodeSnapshot>()
        .checked_add(
            node.raw_vector
                .len()
                .checked_mul(4)
                .ok_or(MemoryError::SizeOverflow)?,
        )
        .and_then(|n| n.checked_add(node.neighbors.len().checked_mul(size_of::<Vec<u64>>())?))
        .ok_or(MemoryError::SizeOverflow)?;
    for layer in &node.neighbors {
        bytes = bytes
            .checked_add(
                layer
                    .len()
                    .checked_mul(8)
                    .ok_or(MemoryError::SizeOverflow)?,
            )
            .ok_or(MemoryError::SizeOverflow)?;
    }
    Ok(bytes)
}

impl HNSWIndex {
    #[cfg(test)]
    pub fn persistence_snapshot(&self) -> HNSWPersistenceDelta {
        let mut delta = self.delta(&StorageReadControl::new(
            &self.memory,
            &uqa_core::CancellationToken::new(),
        ));
        delta.full_rewrite = true;
        delta.collect().unwrap().into_parts().0
    }

    pub fn take_persistence_delta(&mut self) -> HNSWGraphDelta {
        let delta = self.delta(&StorageReadControl::new(
            &self.memory,
            &uqa_core::CancellationToken::new(),
        ));
        self.dirty_nodes.clear();
        self.full_rewrite = false;
        delta
    }

    pub(super) fn delta(&self, control: &StorageReadControl) -> HNSWGraphDelta {
        HNSWGraphDelta {
            meta: self.graph_meta(),
            full_rewrite: self.full_rewrite,
            graph: self.clone(),
            control: control.clone(),
        }
    }

    pub(super) fn graph_meta(&self) -> HNSWGraphMeta {
        HNSWGraphMeta {
            entry_point: self.entry_point,
            max_level: self.max_level,
            next_node_id: self.next_node_id,
            live_count: self.active.len(),
            deleted_count: self.deleted_count,
        }
    }
}

impl From<&HNSWNode> for HNSWNodeSnapshot {
    fn from(node: &HNSWNode) -> Self {
        Self {
            node_id: node.id,
            doc_id: node.doc_id,
            vector_ordinal: node.vector_ordinal,
            raw_vector: node.raw_vector.clone(),
            level: node.level,
            deleted: node.deleted,
            neighbors: node.neighbors.clone(),
        }
    }
}
