//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical graph access shared by engine catalogs and standalone `SQLite`.

use uqa_core::{Edge, Vertex};
use uqa_storage::{GraphEntityFilter, GraphEntityKind};

use crate::{GraphLabelRegistry, GraphStoreResult};

/// Open a write transaction or a nested savepoint on the exact storage session.
pub(crate) fn begin_graph_write(
    backend: std::sync::Arc<dyn uqa_storage::PersistentStorageBackend>,
) -> GraphStoreResult<Box<dyn GraphWriteTransaction>> {
    let savepoint = if backend.in_transaction() {
        let id = uqa_storage::StorageSavepointId::allocate();
        backend.savepoint(id)?;
        Some(id)
    } else {
        backend.begin_transaction()?;
        None
    };
    Ok(Box::new(StorageGraphWriteTransaction {
        backend,
        savepoint,
        active: true,
    }))
}

struct StorageGraphWriteTransaction {
    backend: std::sync::Arc<dyn uqa_storage::PersistentStorageBackend>,
    savepoint: Option<uqa_storage::StorageSavepointId>,
    active: bool,
}

impl GraphWriteTransaction for StorageGraphWriteTransaction {
    fn commit(&mut self) -> GraphStoreResult<()> {
        if let Some(id) = self.savepoint {
            self.backend.release_savepoint(id)?;
        } else {
            self.backend.commit_transaction()?;
        }
        self.active = false;
        Ok(())
    }

    fn rollback(&mut self) -> GraphStoreResult<()> {
        if !self.active {
            return Ok(());
        }
        if let Some(id) = self.savepoint {
            self.backend.rollback_to_savepoint(id)?;
            self.backend.release_savepoint(id)?;
        } else {
            self.backend.rollback_transaction()?;
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for StorageGraphWriteTransaction {
    fn drop(&mut self) {
        if self.active {
            let _ = self.rollback();
        }
    }
}

/// A live storage checkpoint. Implementations also roll back on Drop so a
/// panic cannot publish half a graph mutation.
pub(crate) trait GraphWriteTransaction {
    fn commit(&mut self) -> GraphStoreResult<()>;
    fn rollback(&mut self) -> GraphStoreResult<()>;
}

/// No entity, membership, or adjacency collection is retained by a handle.
/// Multi-read operations run in the caller's pinned storage transaction.
pub(crate) trait GraphStorage: Send + Sync {
    /// Copy only transaction-local write identities when preparing a new
    /// command candidate. The underlying durable snapshots remain shared.
    fn fork_overlay(&self) -> Option<std::sync::Arc<dyn GraphStorage>> {
        None
    }
    fn unmodified_read_snapshot(&self) -> Option<super::PersistentGraphStore> {
        None
    }
    fn begin_write(&self) -> GraphStoreResult<Box<dyn GraphWriteTransaction>>;
    fn graph_names(&self) -> GraphStoreResult<Vec<String>>;
    fn has_graph(&self, graph: &str) -> GraphStoreResult<bool>;
    fn create_graph(&self, graph: &str) -> GraphStoreResult<()>;
    fn delete_graph(&self, graph: &str) -> GraphStoreResult<()>;
    fn registry(&self, graph: &str) -> GraphStoreResult<GraphLabelRegistry>;
    fn save_registry(&self, graph: &str, registry: &GraphLabelRegistry) -> GraphStoreResult<()>;
    fn counter(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>>;
    fn save_counter(&self, kind: GraphEntityKind, next: u64) -> GraphStoreResult<()>;
    fn vertex(&self, id: u64) -> GraphStoreResult<Option<Vertex>>;
    fn edge(&self, id: u64) -> GraphStoreResult<Option<Edge>>;
    fn save_vertex(&self, vertex: &Vertex) -> GraphStoreResult<()>;
    fn save_edge(&self, edge: &Edge) -> GraphStoreResult<()>;
    fn delete_vertex(&self, id: u64) -> GraphStoreResult<()>;
    fn delete_edge(&self, id: u64) -> GraphStoreResult<()>;
    fn ids(
        &self,
        filter: GraphEntityFilter<'_>,
        after: Option<u64>,
        limit: usize,
    ) -> GraphStoreResult<Vec<u64>>;
    fn count(&self, filter: GraphEntityFilter<'_>) -> GraphStoreResult<u64>;
    fn max_id(&self, kind: GraphEntityKind) -> GraphStoreResult<Option<u64>>;
    fn memberships(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<Vec<String>>;
    fn has_membership(&self, kind: GraphEntityKind, id: u64, graph: &str)
        -> GraphStoreResult<bool>;
    fn attach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()>;
    fn detach(&self, kind: GraphEntityKind, id: u64, graph: &str) -> GraphStoreResult<()>;
}
