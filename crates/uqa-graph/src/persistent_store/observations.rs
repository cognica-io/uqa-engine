//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained participant attribution belongs to semantic graph access, not physical decoding or validation reads.

use uqa_core::CancellationToken;
use uqa_storage::catalog::graph_observations::GraphEntityKey;
use uqa_storage::mvcc::{SerializableReadContext, VersionError};
use uqa_storage::{read_control::StorageReadControl, GraphEntityKind};

use super::PersistentGraphStore;
use crate::{GraphStoreError, GraphStoreResult};

#[derive(Clone)]
pub(super) struct GraphRead {
    context: SerializableReadContext,
    control: StorageReadControl,
}

impl PersistentGraphStore {
    /// Bind a query's original participant and allowance without observing data. Clones retain this binding; catalog caches should keep unbound handles and bind each executing reader explicitly.
    pub fn with_serializable_read(
        &self,
        context: SerializableReadContext,
        cancellation: &CancellationToken,
    ) -> Self {
        let control = context.read_control(cancellation);
        let mut store = self.clone();
        store.read = Some(GraphRead { context, control });
        store
    }

    /// Remove a completed query's observer before retaining this handle as session catalog state. Its storage snapshot and resources are unchanged.
    pub fn without_serializable_read(mut self) -> Self {
        self.read = None;
        self
    }

    pub(super) fn observe_entity(&self, kind: GraphEntityKind, id: u64) -> GraphStoreResult<()> {
        if let Some(read) = &self.read {
            read.control.check()?;
            let namespace = self
                .storage
                .entity_observation_namespace(kind, id)?
                .ok_or_else(|| {
                    GraphStoreError::Storage(
                        "serializable graph reads require an immutable entity namespace".into(),
                    )
                })?;
            read.context
                .observe_read(
                    GraphEntityKey::new(namespace, kind, id).predicate(),
                    &read.control,
                )
                .map_err(VersionError::into_storage_error)?;
        }
        Ok(())
    }
}
