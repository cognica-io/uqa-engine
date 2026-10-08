//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence consumers retain durability independently of savepoint undo. Publication admission closes before the owner's completion barrier, so a retained child cannot publish an unsynchronized suffix after its consumer completes.

use super::*;
use parking_lot::{RwLock, RwLockWriteGuard};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct SequenceDurability {
    state: RwLock<SequenceBoundary>,
    required: AtomicBool,
}

pub(super) struct SequenceBoundary {
    generation: u64,
    open: bool,
}

impl Default for SequenceDurability {
    fn default() -> Self {
        Self {
            state: RwLock::new(SequenceBoundary {
                generation: 0,
                open: false,
            }),
            required: AtomicBool::new(false),
        }
    }
}

impl SequenceDurability {
    pub(super) fn begin(&self) -> StorageBackendResult<()> {
        let mut state = self.state.write();
        state.generation = state.generation.checked_add(1).ok_or_else(|| {
            StorageBackendError::Other("sequence completion generations exhausted".into())
        })?;
        state.open = true;
        self.required.store(false, Ordering::Release);
        Ok(())
    }

    pub(super) fn generation(&self) -> u64 {
        self.state.read().generation
    }

    pub(super) fn close(&self) -> RwLockWriteGuard<'_, SequenceBoundary> {
        let mut state = self.state.write();
        state.open = false;
        state
    }

    pub(super) fn clear(&self) {
        self.required.store(false, Ordering::Release);
    }

    fn lease(self: &Arc<Self>, expected: Option<u64>) -> Option<SequencePublicationLease> {
        let state = self.state.read();
        if !state.open || expected.is_some_and(|generation| generation != state.generation) {
            return None;
        }
        self.required.store(true, Ordering::Release);
        Some(SequencePublicationLease {
            owner: Arc::clone(self),
            generation: state.generation,
        })
    }
}

/// A sequence publisher's exact consuming transaction. Its admission lasts through physical publication; owner completion closes admission and waits for every admitted publication before flushing. A child retained past completion or reused after another transaction begins must publish synchronously.
#[derive(Clone)]
pub struct SequencePublicationLease {
    owner: Arc<SequenceDurability>,
    generation: u64,
}

impl SequencePublicationLease {
    /// Hold publication admission while `operation` runs. `true` permits sequence-only WAL deferral; `false` requires the ordinary synchronous path. Acquire this before physical writer admission or connection checkout.
    pub fn with_publication<T>(&self, operation: impl FnOnce(bool) -> T) -> T {
        let state = self.owner.state.read();
        operation(state.open && state.generation == self.generation)
    }
}

impl VersionedKeyValueStore {
    /// Select autonomous sequence publication only while its consuming transaction remains open. Unsupported providers and calls without an active completion owner retain synchronous behavior.
    pub fn new_sequence_value_session(&self, cancellation: &uqa_core::CancellationToken) -> Self {
        let persistence = self
            .sequence_durability
            .lease(self.sequence_generation)
            .and_then(|owner| self.persistence.sequence_value_persistence(owner))
            .unwrap_or_else(|| Arc::clone(&self.persistence));
        Self::new_with_cancellation(
            persistence,
            self.identity.clone(),
            self.options,
            cancellation.clone(),
        )
    }

    /// Sequence values can outlive transaction and savepoint undo; retain their original consumer's completion obligation, including through a retained reader.
    pub fn require_sequence_value_durability(&self) -> StorageBackendResult<()> {
        self.sequence_durability
            .lease(self.sequence_generation)
            .map(|_| ())
            .ok_or_else(|| {
                StorageBackendError::Other(
                    "sequence values require an open consuming transaction".into(),
                )
            })
    }

    pub(super) fn synchronize_sequence_values(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        if self.retained.is_none() && self.sequence_durability.required.load(Ordering::Acquire) {
            self.persistence
                .synchronize_sequence_values(control)
                .map_err(VersionError::into_storage_error)?;
        }
        Ok(())
    }
}

impl Drop for VersionedKeyValueStore {
    fn drop(&mut self) {
        if self.retained.is_none() {
            // An abandoned consumer has no successful completion to acknowledge. Its surviving children must nevertheless stop deferring new publications.
            drop(self.sequence_durability.close());
        }
    }
}
