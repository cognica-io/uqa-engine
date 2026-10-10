//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validated replacement, deletion, and streaming tombstone compaction.

use super::prepare::{check, Control};
use super::types::{active_key, HNSWIndex};
use crate::vector_index::validate_vector_values;
use crate::{StorageBackendError, StorageBackendResult, VectorIndex};
use uqa_core::DocId;

impl HNSWIndex {
    pub(super) fn replace_document_vectors(
        &mut self,
        doc_id: DocId,
        vectors: &[Vec<f32>],
        control: Control<'_>,
    ) -> StorageBackendResult<()> {
        for vector in vectors {
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
        for (ordinal, vector) in vectors.iter().enumerate() {
            check(control)?;
            self.insert_vector(doc_id, ordinal as u32, vector.clone(), control)?;
        }
        self.maybe_rebuild(control)
    }

    pub(super) fn mark_document_deleted(
        &mut self,
        doc_id: DocId,
        control: Control<'_>,
    ) -> StorageBackendResult<()> {
        check(control)?;
        let mut after = active_key(doc_id, 0).checked_sub(1);
        while let Some((key, node_id)) = self.active.next(after)? {
            if key >> 32 != u128::from(doc_id) {
                break;
            }
            let node_id = *node_id;
            if self.node(node_id)?.is_none() {
                return Err(StorageBackendError::Other(format!(
                    "HNSW active map references missing node {node_id}"
                )));
            }
            let deleted = self.deleted_count.checked_add(1).ok_or_else(|| {
                StorageBackendError::Other("HNSW deleted-node counter overflow".into())
            })?;
            self.modify_node(node_id, control, |node| node.deleted = true)?;
            self.active.remove(key, control)?;
            self.deleted_count = deleted;
            after = Some(key);
        }
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
        // The retained root supplies canonical order while the successor is built one vector at a time.
        let source = self.clone();
        self.clear()?;
        for entry in source.active.iter() {
            check(control)?;
            let (key, node_id) = entry?;
            let node = source.node(*node_id)?.ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "HNSW active map references missing node {}",
                    *node_id
                ))
            })?;
            let vector = source.raw_vector(*node_id)?.values.clone();
            drop(node);
            self.insert_vector((key >> 32) as u64, key as u32, vector, control)?;
        }
        Ok(())
    }
}
