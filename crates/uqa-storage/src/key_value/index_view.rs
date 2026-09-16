//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical index caches retain the identity used to evaluate their immutable state.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use parking_lot::Mutex;

use super::{codec::other_error, KeyValueBatch, KeyValueRead, KeyValueReadRevision, KeyValueStore};
use crate::vector_index::VectorIndex;
use crate::StorageBackendResult;

mod snapshot;
pub(super) use snapshot::read_only;

pub(super) struct IndexState<T> {
    pub(super) value: Arc<T>,
    pub(super) snapshot: Arc<dyn VectorIndex>,
    pub(super) revision: Option<u64>,
    pub(super) definition_candidate: bool,
}

impl<T> Clone for IndexState<T> {
    fn clone(&self) -> Self {
        Self {
            value: Arc::clone(&self.value),
            snapshot: Arc::clone(&self.snapshot),
            revision: self.revision,
            definition_candidate: self.definition_candidate,
        }
    }
}

pub(super) struct IndexView<T> {
    cached: Mutex<Option<(KeyValueReadRevision, IndexState<T>)>>,
    preparing_definition: AtomicBool,
}

impl<T: VectorIndex + 'static> IndexView<T> {
    pub(super) fn new(creating: bool) -> Self {
        Self {
            cached: Mutex::new(None),
            preparing_definition: AtomicBool::new(creating),
        }
    }

    pub(super) fn load(
        &self,
        read: &dyn KeyValueRead,
        prefixes: &[&[u8]],
        load: impl FnOnce(bool) -> StorageBackendResult<(T, Option<u64>)>,
    ) -> StorageBackendResult<IndexState<T>> {
        let identity = read.revision(prefixes)?;
        if let Some((cached_identity, state)) = self.cached.lock().as_ref() {
            if *cached_identity == identity {
                return Ok(state.clone());
            }
        }
        let definition_candidate = self.preparing_definition.load(Ordering::Acquire);
        let (value, revision) = load(definition_candidate)?;
        let value = Arc::new(value);
        let state = IndexState {
            snapshot: read_only(value.clone()),
            value,
            revision,
            definition_candidate,
        };
        *self.cached.lock() = Some((identity, state.clone()));
        Ok(state)
    }

    pub(super) fn evaluate(
        &self,
        store: &dyn KeyValueStore,
        operation: impl FnOnce(&dyn KeyValueRead, &mut dyn KeyValueBatch) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        evaluate_mutation(store, operation)?;
        // Creation owns the requested parameters until its first successful staging, including across unrelated changes to the initial view.
        self.preparing_definition.store(false, Ordering::Release);
        Ok(())
    }
}

pub(super) fn read_view<T>(
    store: &dyn KeyValueStore,
    operation: impl FnOnce(&dyn KeyValueRead) -> StorageBackendResult<T>,
) -> StorageBackendResult<T> {
    read_scope(|read| store.with_read_view(read), operation)
}

pub(super) fn read_scope<T>(
    scope: impl FnOnce(&mut super::KeyValueReadScope<'_>) -> StorageBackendResult<()>,
    operation: impl FnOnce(&dyn KeyValueRead) -> StorageBackendResult<T>,
) -> StorageBackendResult<T> {
    let mut operation = Some(operation);
    let mut result = None;
    scope(&mut |read| {
        result = Some(operation.take().ok_or_else(|| {
            other_error("KeyValue provider attempted to replay read evaluation")
        })?(read)?);
        Ok(())
    })?;
    result.ok_or_else(|| other_error("KeyValue provider did not evaluate the read"))
}

pub(super) fn evaluate_mutation(
    store: &dyn KeyValueStore,
    operation: impl FnOnce(&dyn KeyValueRead, &mut dyn KeyValueBatch) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    mutation_scope(|mutate| store.with_mutation(mutate), operation)
}

pub(super) fn mutation_scope(
    scope: impl FnOnce(&mut super::KeyValueMutation<'_>) -> StorageBackendResult<()>,
    operation: impl FnOnce(&dyn KeyValueRead, &mut dyn KeyValueBatch) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    let mut operation = Some(operation);
    scope(&mut |read, batch| {
        operation.take().ok_or_else(|| {
            other_error("KeyValue provider attempted to replay mutation evaluation")
        })?(read, batch)
    })?;
    if operation.is_some() {
        return Err(other_error(
            "KeyValue provider did not evaluate the mutation",
        ));
    }
    Ok(())
}
