//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistence snapshots and incremental dirty-node deltas.

use super::types::{HNSWGraphMeta, HNSWIndex, HNSWNode, HNSWNodeSnapshot, HNSWPersistenceDelta};

impl HNSWIndex {
    #[cfg(test)]
    pub(crate) fn persistence_snapshot(&self) -> HNSWPersistenceDelta {
        HNSWPersistenceDelta {
            meta: self.graph_meta(),
            nodes: self.nodes.values().map(HNSWNodeSnapshot::from).collect(),
            full_rewrite: true,
        }
    }

    pub(crate) fn take_persistence_delta(&mut self) -> HNSWPersistenceDelta {
        let full_rewrite = self.full_rewrite;
        let nodes = if full_rewrite {
            self.nodes.values().map(HNSWNodeSnapshot::from).collect()
        } else {
            self.dirty_nodes
                .iter()
                .filter_map(|node_id| self.nodes.get(node_id))
                .map(HNSWNodeSnapshot::from)
                .collect()
        };
        self.dirty_nodes.clear();
        self.full_rewrite = false;
        HNSWPersistenceDelta {
            meta: self.graph_meta(),
            nodes,
            full_rewrite,
        }
    }

    fn graph_meta(&self) -> HNSWGraphMeta {
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
