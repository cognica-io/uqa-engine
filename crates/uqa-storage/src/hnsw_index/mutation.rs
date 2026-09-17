//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validated replacement, deletion, and tombstone compaction.

use uqa_core::DocId;

use super::prepare::{check, Control};
use super::types::HNSWIndex;
use crate::vector_index::validate_vector_values;
use crate::{StorageBackendError, StorageBackendResult};

impl HNSWIndex {
    pub(super) fn replace_document_vectors(
        &mut self,
        doc_id: DocId,
        vectors: Vec<Vec<f32>>,
        control: Control<'_>,
    ) -> StorageBackendResult<()> {
        for vector in &vectors {
            check(control)?;
            validate_vector_values(self.dimensions, vector)?;
        }
        if u64::try_from(vectors.len()).unwrap_or(u64::MAX) > u64::from(u32::MAX) + 1 {
            return Err(StorageBackendError::Other(
                "HNSW vector ordinal exceeds the u32 index format".into(),
            ));
        }
        let inserted = u64::try_from(vectors.len()).map_err(|_| {
            StorageBackendError::Other("HNSW vector count exceeds the u64 node-id range".into())
        })?;
        self.next_node_id
            .checked_add(inserted)
            .ok_or_else(|| StorageBackendError::Other("HNSW node id space exhausted".into()))?;
        self.mark_document_deleted(doc_id, control)?;
        for (ordinal, vector) in vectors.into_iter().enumerate() {
            check(control)?;
            self.insert_vector(doc_id, ordinal as u32, vector, control)?;
        }
        self.maybe_rebuild(control)
    }

    pub(super) fn mark_document_deleted(
        &mut self,
        doc_id: DocId,
        control: Control<'_>,
    ) -> StorageBackendResult<()> {
        check(control)?;
        let node_ids = self
            .active
            .range((doc_id, 0)..=(doc_id, u32::MAX))
            .map(|(_, node_id)| {
                check(control)?;
                Ok(*node_id)
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        let next_deleted_count =
            self.deleted_count
                .checked_add(node_ids.len())
                .ok_or_else(|| {
                    StorageBackendError::Other("HNSW deleted-node counter overflow".into())
                })?;
        if let Some(node_id) = node_ids
            .iter()
            .find(|node_id| !self.nodes.contains_key(node_id))
        {
            return Err(StorageBackendError::Other(format!(
                "HNSW active map references missing node {node_id}"
            )));
        }
        for node_id in node_ids {
            check(control)?;
            let node = self
                .nodes
                .get_mut(&node_id)
                .expect("node ids were validated before mutation");
            node.deleted = true;
            self.active.remove(&(node.doc_id, node.vector_ordinal));
            self.dirty_nodes.insert(node_id);
        }
        self.deleted_count = next_deleted_count;
        Ok(())
    }

    pub(super) fn maybe_rebuild(&mut self, control: Control<'_>) -> StorageBackendResult<()> {
        check(control)?;
        if self.deleted_count >= self.params.rebuild_threshold {
            self.rebuild(control)?;
        }
        Ok(())
    }

    fn rebuild(&mut self, control: Control<'_>) -> StorageBackendResult<()> {
        let live = self
            .active
            .iter()
            .map(|(&(doc_id, ordinal), node_id)| {
                check(control)?;
                self.nodes
                    .get(node_id)
                    .map(|node| (doc_id, ordinal, node.raw_vector.clone()))
                    .ok_or_else(|| {
                        StorageBackendError::Other(format!(
                            "HNSW active map references missing node {node_id}"
                        ))
                    })
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        self.nodes.clear();
        self.active.clear();
        self.entry_point = None;
        self.max_level = 0;
        self.next_node_id = 1;
        self.deleted_count = 0;
        self.dirty_nodes.clear();
        self.full_rewrite = true;
        for (doc_id, ordinal, vector) in live {
            check(control)?;
            self.insert_vector(doc_id, ordinal, vector, control)?;
        }
        Ok(())
    }
}
