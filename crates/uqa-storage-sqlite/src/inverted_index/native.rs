//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native occurrence rows adapt the common algorithms to one logical transaction boundary.

mod mutation;
mod read;
mod records;

use std::sync::Arc;
use uqa_storage::key_value::{
    KeyValueInvertedIndex, KeyValueMutation, KeyValueReadScope, OccurrenceStorage,
};
use uqa_storage::StorageBackendResult;

use super::SQLiteInvertedIndex;
use crate::connection::{ManagedConnection, SQLiteError};
use mutation::ProjectionBatch;
use read::NativeRead;

struct NativeOccurrenceStorage {
    connection: ManagedConnection,
    table: String,
}

impl OccurrenceStorage for NativeOccurrenceStorage {
    fn read(&self, operation: &mut KeyValueReadScope<'_>) -> StorageBackendResult<()> {
        let snapshot = self.connection.native_snapshot()?.ok_or_else(|| {
            records::invalid("native occurrence storage requires a logical session")
        })?;
        operation(&NativeRead::new(&snapshot, &self.table)?)
    }

    fn mutate(&self, operation: &mut KeyValueMutation<'_>) -> StorageBackendResult<()> {
        self.connection
            .with_native_write(|snapshot, batch| {
                let read = NativeRead::new(snapshot, &self.table)?;
                let mut projected = ProjectionBatch::new(&read, batch);
                operation(&read, &mut projected)?;
                projected.flush().map_err(SQLiteError::from)
            })?
            .ok_or_else(|| records::invalid("native occurrence storage requires a logical session"))
    }
}

impl SQLiteInvertedIndex {
    pub(super) fn native_index(&self) -> Option<KeyValueInvertedIndex> {
        self.conn.is_native_record_session().then(|| {
            KeyValueInvertedIndex::from_storage(
                Arc::new(NativeOccurrenceStorage {
                    connection: self.conn.clone(),
                    table: self.table.clone(),
                }),
                self.table.clone(),
                self.bindings.clone(),
            )
        })
    }
}
