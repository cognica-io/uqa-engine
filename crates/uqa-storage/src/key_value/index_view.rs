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
use uqa_core::memory::Budgeted;

use super::{codec::other_error, KeyValueBatch, KeyValueRead, KeyValueReadRevision, KeyValueStore};
use crate::vector_index::VectorIndex;
use crate::{ReadOnlySnapshot, StorageBackendResult};

#[cfg(test)]
mod tests;

pub(super) struct IndexState<T> {
    pub(super) value: ReadOnlySnapshot<T>,
    pub(super) snapshot: Arc<dyn VectorIndex>,
    pub(super) revision: Option<u64>,
    pub(super) definition_candidate: bool,
    control: Option<crate::read_control::StorageReadControl>,
}

impl<T> Clone for IndexState<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            snapshot: Arc::clone(&self.snapshot),
            revision: self.revision,
            definition_candidate: self.definition_candidate,
            control: self.control.clone(),
        }
    }
}

impl<T: VectorIndex + 'static> IndexState<T> {
    fn for_read(
        &mut self,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<Self> {
        // Keep the physical cache value unbound. Reuse a reader only when its allowance and cancellation signal match, so an independent caller never inherits another reader's cancellation.
        let mut reader = self.clone();
        reader.value = reader.value.with_vector_read_control(control)?;
        if !self
            .control
            .as_ref()
            .is_some_and(|cached| cached.shares_context(control))
        {
            self.snapshot = reader.value.snapshot()?;
            self.control = Some(control.clone());
        }
        reader.snapshot = Arc::clone(&self.snapshot);
        reader.control = self.control.clone();
        Ok(reader)
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
        load: impl FnOnce(bool) -> StorageBackendResult<(Budgeted<T>, Option<u64>)>,
    ) -> StorageBackendResult<IndexState<T>> {
        read.control().check()?;
        let identity = read.revision(prefixes)?;
        if let Some((cached_identity, state)) = self.cached.lock().as_mut() {
            if *cached_identity == identity {
                return state.for_read(read.control());
            }
        }
        let definition_candidate = self.preparing_definition.load(Ordering::Acquire);
        let (value, revision) = load(definition_candidate)?;
        let value = ReadOnlySnapshot::from_budgeted(value)?;
        let mut state = IndexState {
            snapshot: value.snapshot()?,
            value,
            revision,
            definition_candidate,
            control: None,
        };
        let reader = state.for_read(read.control())?;
        *self.cached.lock() = Some((identity, state));
        Ok(reader)
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

pub(super) fn evaluate_mutation<T>(
    store: &dyn KeyValueStore,
    operation: impl FnOnce(&dyn KeyValueRead, &mut dyn KeyValueBatch) -> StorageBackendResult<T>,
) -> StorageBackendResult<T> {
    mutation_scope(|mutate| store.with_mutation(mutate), operation)
}

pub(super) fn mutation_scope<T>(
    scope: impl FnOnce(&mut super::KeyValueMutation<'_>) -> StorageBackendResult<()>,
    operation: impl FnOnce(&dyn KeyValueRead, &mut dyn KeyValueBatch) -> StorageBackendResult<T>,
) -> StorageBackendResult<T> {
    let mut operation = Some(operation);
    let mut result = None;
    scope(&mut |read, batch| {
        result = Some(operation.take().ok_or_else(|| {
            other_error("KeyValue provider attempted to replay mutation evaluation")
        })?(read, batch)?);
        Ok(())
    })?;
    result.ok_or_else(|| other_error("KeyValue provider did not evaluate the mutation"))
}
