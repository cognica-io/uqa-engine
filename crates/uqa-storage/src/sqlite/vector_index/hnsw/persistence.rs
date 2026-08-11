//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Revision-aware immutable graph cache publication.

use std::sync::Arc;

use super::{CachedGraph, SQLiteHNSWIndex};
use crate::hnsw_index::HNSWIndex;
use crate::{StorageBackendError, StorageBackendResult};

impl SQLiteHNSWIndex {
    pub(super) fn cached_graph_for_revision(
        &self,
        revision: u64,
    ) -> StorageBackendResult<Arc<HNSWIndex>> {
        self.cached_graph_state_for_revision(revision)
            .map(|cached| cached.graph)
    }

    pub(super) fn cached_graph_state(&self) -> StorageBackendResult<CachedGraph> {
        let revision = self.persisted_revision()?.ok_or_else(|| {
            StorageBackendError::Other(format!(
                "missing persisted HNSW metadata for {}.{}",
                self.persistent.table, self.persistent.field
            ))
        })?;
        self.cached_graph_state_for_revision(revision)
    }

    fn cached_graph_state_for_revision(&self, revision: u64) -> StorageBackendResult<CachedGraph> {
        if let Some(cached) = self.graph.read().as_ref() {
            if cached.revision == revision {
                return Ok(cached.clone());
            }
        }
        let (loaded_revision, loaded_graph) = self.load_graph()?;
        let loaded = CachedGraph {
            revision: loaded_revision,
            graph: Arc::new(loaded_graph),
        };
        if self.persisted_revision()? != Some(loaded_revision) {
            return Ok(loaded);
        }
        let mut graph = self.graph.write();
        if let Some(existing) = graph.as_ref() {
            if existing.revision == loaded_revision {
                return Ok(existing.clone());
            }
        }
        *graph = Some(loaded.clone());
        Ok(loaded)
    }

    pub(super) fn publish_graph(&self, graph: HNSWIndex, revision: u64) {
        *self.graph.write() = Some(CachedGraph {
            revision,
            graph: Arc::new(graph),
        });
    }

    pub(super) fn persisted_revision(&self) -> StorageBackendResult<Option<u64>> {
        Ok(self.load_meta()?.map(|(_, _, _, revision)| revision))
    }
}
