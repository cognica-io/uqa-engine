//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable graph caches identify both the metadata revision and its evaluated physical view.

use std::sync::Arc;

use super::{loading::load_meta_from, CachedGraph, GraphIdentity, SQLiteHNSWIndex};
use crate::connection::SnapshotIdentity;
use crate::Result;
use uqa_storage::hnsw_index::HNSWIndex;
use uqa_storage::{ReadOnlySnapshot, StorageBackendResult};

impl SQLiteHNSWIndex {
    pub(super) fn graph_snapshot(&self) -> StorageBackendResult<Option<CachedGraph>> {
        Ok(self
            .persistent
            .conn
            .with_snapshot(|connection, identity| {
                let Some((_, _, _, revision)) = load_meta_from(connection, self)? else {
                    return Ok(None);
                };
                self.cached_graph_at(connection, identity, revision)
                    .map(Some)
            })?
            .0)
    }

    pub(super) fn cached_graph_at(
        &self,
        connection: &rusqlite::Connection,
        identity: &SnapshotIdentity,
        revision: u64,
    ) -> Result<CachedGraph> {
        if let Some(cached) = self.graph.read().as_ref() {
            if cached.revision == revision
                && matches!(&cached.identity, GraphIdentity::Physical(view) if view.same_view(identity))
            {
                return Ok(cached.clone());
            }
        }
        let (revision, graph) = self.load_graph_from(connection)?;
        let loaded = CachedGraph {
            revision,
            identity: GraphIdentity::Physical(identity.clone()),
            graph: ReadOnlySnapshot::new(Arc::new(graph)),
        };
        *self.graph.write() = Some(loaded.clone());
        Ok(loaded)
    }

    pub(super) fn publish_graph(
        &self,
        graph: HNSWIndex,
        revision: u64,
        identity: SnapshotIdentity,
    ) {
        *self.graph.write() = Some(CachedGraph {
            revision,
            identity: GraphIdentity::Physical(identity),
            graph: ReadOnlySnapshot::new(Arc::new(graph)),
        });
    }

    #[cfg(test)]
    pub(super) fn persisted_revision(&self) -> StorageBackendResult<Option<u64>> {
        Ok(self.load_meta()?.map(|(_, _, _, revision)| revision))
    }
}
