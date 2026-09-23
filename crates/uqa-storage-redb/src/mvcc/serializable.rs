//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exclusive redb owners persist the common SSI checkpoint between short local admissions.

mod schema;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, Weak},
};

use redb::Database;
use uqa_storage::{
    mvcc::{
        DatabaseId, LocalSerializableState, SerializableCoordinator, SerializableOperation,
        VersionError, VersionResult, VersionedPersistence,
    },
    read_control::{CancellationToken, StorageReadControl},
};

use super::RedbRecordStore;

pub(super) fn validate_restore(
    store: &RedbRecordStore,
    control: &StorageReadControl,
) -> VersionResult<()> {
    schema::load(store, control)?
        .graph
        .validate_persisted_publications(control, |id| store.commit_status(id, control))
}

pub(super) fn clear_restored_state(transaction: &redb::WriteTransaction) -> VersionResult<()> {
    schema::clear_restored_state(transaction)
}

type Registries = HashMap<(usize, bool), (DatabaseId, Weak<LocalSerializableState>)>;

pub(super) fn registry(
    database: &Arc<Database>,
    identity: DatabaseId,
    receipt_namespace: bool,
) -> VersionResult<Arc<LocalSerializableState>> {
    static REGISTRIES: OnceLock<Mutex<Registries>> = OnceLock::new();
    let key = (Arc::as_ptr(database) as usize, receipt_namespace);
    let mut registries = REGISTRIES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registries.retain(|_, (_, state)| state.strong_count() != 0);
    if let Some((old_identity, state)) = registries.get(&key) {
        if let Some(state) = state.upgrade() {
            if *old_identity != identity {
                return Err(VersionError::WrongDatabase);
            }
            return Ok(state);
        }
    }
    // Live participants retain this state and the Database itself. When no handles remain, the durable checkpoint still preserves the coordinator and allocation watermark.
    let state = Arc::new(LocalSerializableState::default());
    registries.insert(key, (identity, Arc::downgrade(&state)));
    Ok(state)
}

impl SerializableCoordinator for RedbRecordStore {
    fn with_serializable_admission(
        &self,
        control: &StorageReadControl,
        operation: &mut SerializableOperation<'_>,
    ) -> VersionResult<()> {
        self.serializable
            .with_admission(&self.database, control, |leases| {
                let mut loaded = schema::load(self, control)?;
                // Loading closes its physical read before receipt recovery, snapshot capture or publication. No physical writer spans the operation.
                loaded
                    .graph
                    .validate_persisted_publications(control, |transaction| {
                        self.commit_status(transaction, control)
                    })?;
                let result = operation(&mut loaded.graph, leases);
                let finish = StorageReadControl::new(control.memory(), &CancellationToken::new());
                schema::persist(self, &loaded, &finish)?;
                result
            })
    }
}
