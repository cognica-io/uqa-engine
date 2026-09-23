//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An exclusively reopened backup changes history and retires old receipts and SSI state in one durable transaction.

use std::sync::Arc;

use redb::{Database, ReadableDatabase};
use uqa_storage::{
    mvcc::{DatabaseRestore, VersionResult},
    read_control::StorageReadControl,
};

use super::{codec, physical_writer, serializable, RedbRecordStore, METADATA, TRANSACTIONS};
use crate::error::redb_error;

/// The caller opens this Database exclusively and has not exposed any sessions, snapshots or participants from it.
pub(crate) fn open(
    database: Database,
    request: DatabaseRestore,
    control: &StorageReadControl,
) -> VersionResult<RedbRecordStore> {
    control.cancellation().check()?;
    {
        // Check before initialization or format migration, including rejection of an unrelated or uninitialized file.
        let transaction = database.begin_read().map_err(redb_error)?;
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        request.needs_restore(codec::database_id(&metadata)?)?;
    }
    let database = Arc::new(database);
    let store = RedbRecordStore::new(Arc::clone(&database))?;
    if !request.needs_restore(store.identity)? {
        return Ok(store);
    }
    publish(&store, request, control)?;
    // Old registries must disappear before the restored incarnation receives its own admission owners.
    drop(store);
    RedbRecordStore::new(database)
}

pub(super) fn publish(
    store: &RedbRecordStore,
    request: DatabaseRestore,
    control: &StorageReadControl,
) -> VersionResult<()> {
    control.cancellation().check()?;
    if !request.needs_restore(store.identity)? {
        return Ok(());
    }
    serializable::validate_restore(store, control)?;
    let transaction = physical_writer(&store.database)?;
    {
        let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        codec::validate_metadata(&metadata, request.source())?;
        metadata
            .insert("database", request.target().as_bytes().as_slice())
            .map_err(redb_error)?;
    }
    // Data keys, committed revisions and identifier watermarks remain unchanged. No receipt from the backup is relabeled as a transaction in the new history.
    transaction.delete_table(TRANSACTIONS).map_err(redb_error)?;
    transaction.open_table(TRANSACTIONS).map_err(redb_error)?;
    serializable::clear_restored_state(&transaction)?;
    control.cancellation().check()?;
    transaction.commit().map_err(redb_error)?;
    Ok(())
}
