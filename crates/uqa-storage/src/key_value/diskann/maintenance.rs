//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A finite discovery snapshot and bounded current-state deletion share the owning provider.

use std::sync::Arc;

use uqa_core::memory::MemoryReservation;

use crate::diskann_index::format::DiskANNGeneration;
use crate::key_value::KeyValueRead;
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendResult};

use super::{
    invalid,
    keys::{generation_prefix, state_generation, Keys, ROOT},
    read_data_identity,
    reclamation::{Reclamation, ReclamationMode},
    source::KEY_PAGE_LIMIT,
    validate_session, versioned_database, KeyValueDiskANNStore,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskANNMaintenanceStatus {
    /// A live build, retained source or selected generation prevents deletion in this pass.
    Retained,
    /// One bounded deletion committed; this same generation needs another step.
    MoreRecords,
    /// No current records remain for the generation.
    Reclaimed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskANNMaintenanceStep {
    pub generation: DiskANNGeneration,
    pub status: DiskANNMaintenanceStatus,
}

/// Discover only generations visible at startup, without loading artifact payloads or building an identity directory. Each step checks one current generation and deletes at most 64 payload keys. New and earlier generations are reconsidered by the next pass. Errors preserve the current cursor and original physical attempt.
pub struct KeyValueDiskANNMaintenance {
    repository: Option<KeyValueDiskANNStore>,
    read: Option<Arc<dyn KeyValueRead + Send + Sync>>,
    database: Option<[u8; 16]>,
    after: Option<DiskANNGeneration>,
    current: Option<DiskANNGeneration>,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

impl KeyValueDiskANNMaintenance {
    pub fn start(
        store: &Arc<dyn KeyValueStore>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        versioned_database(&**store)?;
        let memory = control.memory().reserve(std::mem::size_of::<Self>())?;
        let session = store.open_session_with_cancellation(control.cancellation())?;
        validate_session(&**store, &*session)?;
        if session.in_transaction() {
            return Err(invalid(
                "maintenance requires an independent inactive session",
            ));
        }
        let retained = session.open_retained_read_session(control.cancellation())?;
        validate_session(&*session, &*retained)?;
        let mut read = None;
        retained.with_read_view(&mut |view| {
            read = Some(view.retain(&[ROOT])?);
            Ok(())
        })?;
        let read = read.ok_or_else(|| invalid("maintenance did not retain a read boundary"))?;
        let database = read_data_identity(&*read, control)?;
        if database.is_none() && read.contains_prefix_budgeted(ROOT, control)? {
            return Err(invalid("generation records have no data identity"));
        }
        let repository = database
            .map(|_| KeyValueDiskANNStore::connect(&session, control))
            .transpose()?;
        Ok(Self {
            repository,
            read: database.map(|_| read),
            database,
            after: None,
            current: None,
            control: control.clone(),
            _memory: memory,
        })
    }

    /// `None` completes this finite pass and releases its discovery snapshot. Retained generations do not prevent reaching later generations; incomplete deletion stays on its current generation.
    pub fn step(&mut self) -> StorageBackendResult<Option<DiskANNMaintenanceStep>> {
        self.control.check()?;
        if self.current.is_none() {
            self.current = self.next_generation()?;
        }
        let Some(generation) = self.current else {
            self.read = None;
            return Ok(None);
        };
        let repository = self
            .repository
            .as_ref()
            .expect("discovered data has a repository");
        let reclaimed = repository.reclaim_generation(
            generation,
            KEY_PAGE_LIMIT,
            ReclamationMode::Maintenance,
            &self.control,
        )?;
        let status = match reclaimed {
            Reclamation::Complete => DiskANNMaintenanceStatus::Reclaimed,
            Reclamation::More => DiskANNMaintenanceStatus::MoreRecords,
            Reclamation::Retained => DiskANNMaintenanceStatus::Retained,
        };
        if reclaimed != Reclamation::More {
            self.after = Some(generation);
            self.current = None;
        }
        Ok(Some(DiskANNMaintenanceStep { generation, status }))
    }

    /// Resolve the exact original attempt before resuming a failed step. No discovery or deletion callback is replayed by this operation.
    pub fn commit_pending(&self) -> StorageBackendResult<()> {
        if let Some(repository) = &self.repository {
            repository.commit_pending()?;
        }
        Ok(())
    }

    pub fn rollback_pending(&self) -> StorageBackendResult<()> {
        if let Some(repository) = &self.repository {
            repository.rollback_pending()?;
        }
        Ok(())
    }

    /// Drain one finite pass for an existing storage-maintenance invocation. Every step keeps the same per-batch bound; dropping the pass releases discovery history before ordinary provider version reclamation.
    pub fn run(
        store: &Arc<dyn KeyValueStore>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let mut pass = Self::start(store, control)?;
        while pass.step()?.is_some() {}
        super::KeyValueDiskANNMappingMaintenance::run(store, control)
    }

    fn next_generation(&self) -> StorageBackendResult<Option<DiskANNGeneration>> {
        let Some(read) = &self.read else {
            return Ok(None);
        };
        let database = self.database.expect("retained discovery data identity");
        let after = self
            .after
            .map(|generation| Keys::new(generation).after_generation());
        let mut found = None;
        let mut failure = None;
        read.control().check()?;
        let source = read.visit_keys_after(
            &generation_prefix(database),
            after.as_ref().map(AsRef::as_ref),
            1,
            &self.control,
            &mut |key| {
                if failure.is_some() {
                    return Err(invalid("generation discovery already failed"));
                }
                let outcome = (|| {
                    self.control.check()?;
                    read.control().check()?;
                    if found.is_some() {
                        return Err(invalid("generation discovery exceeded its key limit"));
                    }
                    let generation = state_generation(key)?;
                    if generation.database() != database
                        || self.after.is_some_and(|previous| {
                            Keys::new(previous).prefix() >= Keys::new(generation).prefix()
                        })
                    {
                        return Err(invalid(
                            "generation discovery returned an unordered or foreign identity",
                        ));
                    }
                    found = Some(generation);
                    Ok(())
                })();
                if let Err(error) = outcome {
                    failure = Some(error);
                    return Err(invalid("generation discovery rejected a key"));
                }
                Ok(())
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
        source?;
        read.control().check()?;
        self.control.check()?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests;
