//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database-owned immutable `DiskANN` generations above the common versioned byte store.

use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::memory::Budgeted;

use crate::catalog::new_nonzero_catalog_identity;
use crate::diskann_index::format::DiskANNGeneration;
use crate::mvcc::{DatabaseId, IdentifierRequest};
use crate::read_control::StorageReadControl;
use crate::{
    KeyValueStore, StorageBackendError, StorageBackendResult, StorageSessionAffinity,
    StorageTransactionModel,
};

use super::{KeyValueMutation, KeyValueRead};

pub(super) mod conformance;
mod identity;
mod keys;
pub mod publication;
mod source;
mod staging;
mod state;

use keys::{database_key, Keys};
use state::{data_identity, fixed};

pub use source::KeyValueDiskANNSource;
pub use staging::KeyValueDiskANNStage;
pub use state::DiskANNStageStatus;

struct Owner {
    store: Arc<dyn KeyValueStore>,
    database: DatabaseId,
    affinity: StorageSessionAffinity,
    writer: Mutex<()>,
}

/// A dedicated physical staging session. Its completion methods resolve existing evaluated attempts; they never replay a mutation or complete the caller's SQL transaction.
#[derive(Clone)]
pub struct KeyValueDiskANNStore {
    owner: Arc<Budgeted<Owner>>,
}

impl KeyValueDiskANNStore {
    /// Allocate a physical generation through durable mappings of the captured catalog incarnations. This only prepares staging; publication still requires the retained source's current-definition guard and complete build coverage.
    pub fn allocate_bound_stage(
        &self,
        scope: &crate::diskann_index::catalog::DiskANNIndexScope,
        control: &StorageReadControl,
    ) -> StorageBackendResult<KeyValueDiskANNStage> {
        let (database, table, index) = self.catalog_handles(scope, control)?;
        let stage = self.allocate_stage(table, index, control)?;
        scope.check(self.owner.database, control)?;
        if stage.generation().database() != database {
            return Err(invalid(
                "data identity changed during bound generation allocation",
            ));
        }
        Ok(stage)
    }

    pub fn connect(
        source: &Arc<dyn KeyValueStore>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let database = versioned_database(&**source)?;
        let store = source.open_session_with_cancellation(control.cancellation())?;
        let affinity = validate_session(&**source, &*store)?;
        if store.in_transaction() || store.identifier_allocator().is_none() {
            return Err(invalid(
                "staging requires an inactive session and durable identifiers",
            ));
        }
        // Verify retained reads before any initialization writes. The temporary lease does not enumerate records.
        let retained = store.open_retained_read_session(control.cancellation())?;
        validate_session(&*store, &*retained)?;
        drop(retained);
        let owner = Budgeted::new(
            Owner {
                store,
                database,
                affinity,
                writer: Mutex::new(()),
            },
            control.memory().empty_reservation(),
        )
        .into_shared()?;
        Ok(Self { owner })
    }

    /// Establish a persistent data identity once. It remains separate from transaction-history identity across backup restoration.
    pub fn initialize(&self, control: &StorageReadControl) -> StorageBackendResult<[u8; 16]> {
        let mut selected = None;
        self.mutate(control, &mut |read, batch| {
            selected = read_data_identity(read, control)?;
            if selected.is_none() {
                let identity = new_nonzero_catalog_identity("DiskANN", "data")?;
                let mut bytes = [1; 17];
                bytes[1..].copy_from_slice(&identity);
                batch.put(&database_key(), &bytes)?;
                selected = Some(identity);
            }
            Ok(())
        })?;
        selected.ok_or_else(|| invalid("initialization did not select a data identity"))
    }

    pub fn data_identity(&self, control: &StorageReadControl) -> StorageBackendResult<[u8; 16]> {
        let _writer = self.owner.writer.lock();
        self.idle(control)?;
        stored_data_identity(&*self.owner.store, control)
    }

    /// Reserve a never-reused generation number. Starting the returned handle is a separate fallible operation, so the caller retains its identity if creation loses its commit reply.
    pub fn allocate_stage(
        &self,
        table: u64,
        index: u64,
        control: &StorageReadControl,
    ) -> StorageBackendResult<KeyValueDiskANNStage> {
        let _writer = self.owner.writer.lock();
        self.idle(control)?;
        let database = stored_data_identity(&*self.owner.store, control)?;
        let provisional = DiskANNGeneration::new(database, table, index, 1)?;
        let keys = Keys::new(provisional);
        let allocation = self
            .owner
            .store
            .identifier_allocator()
            .ok_or_else(|| invalid("durable generation identifiers are unavailable"))?
            .allocate_identifiers(
                keys.allocation_namespace(),
                IdentifierRequest::Reserve {
                    minimum: 1,
                    maximum: u64::MAX,
                    count: std::num::NonZeroU64::MIN,
                },
            )?;
        let generation = DiskANNGeneration::new(database, table, index, allocation.watermark())?;
        Ok(KeyValueDiskANNStage::reserved(
            self.clone(),
            generation,
            new_nonzero_catalog_identity("DiskANN", "staging owner")?,
        ))
    }

    /// Reopen an existing staging owner; an absent or discarded generation cannot be recreated by this path.
    pub fn resume_stage(
        &self,
        generation: DiskANNGeneration,
        control: &StorageReadControl,
    ) -> StorageBackendResult<KeyValueDiskANNStage> {
        KeyValueDiskANNStage::resume(self.clone(), generation, control)
    }

    pub fn open_source(
        &self,
        generation: DiskANNGeneration,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<KeyValueDiskANNSource>> {
        KeyValueDiskANNSource::capture(self, generation, DiskANNStageStatus::Sealed, control)
    }

    /// Finish the exact retained attempt, including acknowledgement after a durable commit with a lost reply.
    pub fn commit_pending(&self) -> StorageBackendResult<()> {
        let _writer = self.owner.writer.lock();
        if self.owner.store.in_transaction() {
            self.owner.store.commit_transaction()?;
        }
        Ok(())
    }

    /// Resolve or abort the exact retained attempt. A confirmed durable commit remains a committed outcome from the underlying session.
    pub fn rollback_pending(&self) -> StorageBackendResult<()> {
        let _writer = self.owner.writer.lock();
        if self.owner.store.in_transaction() {
            self.owner.store.rollback_transaction()?;
        }
        Ok(())
    }

    fn idle(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        control.check()?;
        if versioned_database(&*self.owner.store)? != self.owner.database
            || self.owner.store.transaction_affinity().as_ref() != Some(&self.owner.affinity)
        {
            return Err(invalid("staging session changed its database or affinity"));
        }
        if self.owner.store.in_transaction() {
            return Err(invalid(
                "resolve the retained staging attempt before another operation",
            ));
        }
        Ok(())
    }

    fn mutate(
        &self,
        control: &StorageReadControl,
        operation: &mut KeyValueMutation<'_>,
    ) -> StorageBackendResult<()> {
        let _writer = self.owner.writer.lock();
        self.idle(control)?;
        self.owner.store.with_mutation(&mut |read, batch| {
            operation(read, batch)?;
            control.check()
        })
    }
}

fn versioned_database(store: &dyn KeyValueStore) -> StorageBackendResult<DatabaseId> {
    match store.transaction_model() {
        StorageTransactionModel::VersionedConcurrent { database }
            if store.transaction_affinity().is_some() =>
        {
            Ok(database)
        }
        _ => Err(invalid(
            "versioned session and retention capabilities are required",
        )),
    }
}

fn validate_session(
    source: &dyn KeyValueStore,
    retained: &dyn KeyValueStore,
) -> StorageBackendResult<StorageSessionAffinity> {
    if versioned_database(source)? != versioned_database(retained)?
        || source.storage_identity()? != retained.storage_identity()?
        || source.transaction_affinity() == retained.transaction_affinity()
    {
        return Err(invalid(
            "independent session has a different provider or shared affinity",
        ));
    }
    retained
        .transaction_affinity()
        .ok_or_else(|| invalid("session affinity is missing"))
}

fn stored_data_identity(
    store: &dyn KeyValueStore,
    control: &StorageReadControl,
) -> StorageBackendResult<[u8; 16]> {
    fixed(control, |visit| {
        store.visit_value_bounded(&database_key(), 17, control, visit)
    })?
    .map(data_identity)
    .transpose()?
    .ok_or_else(|| invalid("persistent data identity is missing"))
}

fn read_data_identity(
    read: &dyn KeyValueRead,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<[u8; 16]>> {
    fixed(control, |visit| {
        read.visit_value_bounded(&database_key(), 17, control, visit)
    })?
    .map(data_identity)
    .transpose()
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid DiskANN Key/Value generation: {message}"))
}
