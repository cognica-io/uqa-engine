//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Empty vector fields retire reference markers without changing their lifetime observations.

use std::sync::Arc;

use uqa_core::memory::{BudgetedVec, MemoryReservation};

use super::VersionedKeyValueStore;
use crate::mvcc::{CommittedRecordSnapshot, VectorFieldGuard, VersionError};
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendError, StorageBackendResult};

/// A finite key-only discovery pass. Each step revisits one current field and conditionally deletes its reference marker. Retained readers keep the old records; ordinary document writers retain their lifetime observation and mergeable markers. Uncertain commits keep the current key and original attempt until explicit completion.
pub struct VectorFieldGuardMaintenance {
    writer: VersionedKeyValueStore,
    read: Option<Arc<dyn CommittedRecordSnapshot>>,
    prefix: BudgetedVec<u8>,
    after: BudgetedVec<u8>,
    current: BudgetedVec<u8>,
    guard: Option<VectorFieldGuard>,
    completed: Option<bool>,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

impl VectorFieldGuardMaintenance {
    pub fn start(
        store: &VersionedKeyValueStore,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(std::mem::size_of::<Self>())?;
        let prefix = store
            .persistence
            .vector_field_guard_layout()
            .prefix(control)
            .map_err(VersionError::into_storage_error)?;
        let read = store
            .persistence
            .snapshot(control)
            .map_err(VersionError::into_storage_error)?;
        Ok(Self {
            writer: store.new_controlled_session(control),
            read: Some(read),
            prefix,
            after: BudgetedVec::new(control.memory()),
            current: BudgetedVec::new(control.memory()),
            guard: None,
            completed: None,
            control: control.clone(),
            _memory: memory,
        })
    }

    /// `None` completes discovery. `Some(true)` deletes the current empty field's live marker; `Some(false)` leaves a live field, a tombstone, or a racing writer for later maintenance.
    pub fn step(&mut self) -> StorageBackendResult<Option<bool>> {
        self.control.check()?;
        if self.writer.in_transaction() {
            return Err(VersionError::InvalidEncoding(
                "resolve the original field-guard maintenance attempt before advancing",
            )
            .into_storage_error());
        }
        if self.current.is_empty() {
            let Some(read) = &self.read else {
                return Ok(None);
            };
            let mut selected = BudgetedVec::new(self.control.memory());
            let mut guard = None;
            read.visit_keys(
                &self.prefix,
                (!self.after.is_empty()).then_some(&*self.after),
                1,
                &self.control,
                &mut |key, metadata| {
                    selected.extend_from_slice(key)?;
                    if metadata.live {
                        guard = self
                            .writer
                            .persistence
                            .vector_field_guard_layout()
                            .reference(key, &self.control)?;
                    }
                    Ok(false)
                },
            )
            .map_err(VersionError::into_storage_error)?;
            self.control.check()?;
            self.current = selected;
            self.guard = guard;
            if self.current.is_empty() {
                self.read = None;
                return Ok(None);
            }
        }
        let mut removed = false;
        if let Some(guard) = self.guard.as_ref().filter(|_| self.completed.is_none()) {
            let result = self.writer.with_mutation(&mut |read, batch| {
                let mut present = false;
                read.visit_value_budgeted(&guard.references, &self.control, &mut |value| {
                    if let Some(value) = value {
                        if value != &*guard.reference_value {
                            return Err(VersionError::InvalidEncoding(
                                "vector field reference marker payload changed",
                            )
                            .into_storage_error());
                        }
                        present = true;
                    }
                    Ok(())
                })?;
                if present && !read.contains_prefix_budgeted(&guard.vectors, &self.control)? {
                    read.visit_value_budgeted(&guard.lifetime, &self.control, &mut |value| {
                        if value.is_some() {
                            return Err(VersionError::InvalidEncoding(
                                "vector field lifetime guard has a live payload",
                            )
                            .into_storage_error());
                        }
                        Ok(())
                    })?;
                    // Cleanup changes no canonical vectors, so it must not manufacture a structural lifetime transition that rejects an ordinary writer.
                    batch.require_unchanged(&guard.lifetime)?;
                    batch.delete(&guard.references)?;
                    removed = true;
                }
                Ok(())
            });
            match result {
                Err(StorageBackendError::Backend { source, .. })
                    if matches!(
                        source.downcast_ref::<VersionError>(),
                        Some(
                            VersionError::WriteConflict { .. } | VersionError::ReadConflict { .. }
                        )
                    ) =>
                {
                    self.writer.rollback_transaction()?;
                    removed = false;
                }
                result => result?,
            }
            self.completed = Some(removed);
        }
        let removed = self.completed.unwrap_or(false);
        self.after.clear();
        self.after.extend_from_slice(&self.current)?;
        self.current.clear();
        self.guard = None;
        self.completed = None;
        Ok(Some(removed))
    }

    /// Resolve the retained original commit; the next step advances the cursor without reevaluating the completed field.
    pub fn commit_pending(&mut self) -> StorageBackendResult<()> {
        self.writer.commit_transaction()?;
        self.completed = Some(true);
        Ok(())
    }

    pub fn rollback_pending(&self) -> StorageBackendResult<()> {
        self.writer.rollback_transaction()
    }

    pub fn run(
        store: &VersionedKeyValueStore,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let mut pass = Self::start(store, control)?;
        while pass.step()?.is_some() {}
        Ok(())
    }
}
