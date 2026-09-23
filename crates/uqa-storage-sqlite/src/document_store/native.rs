//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document operations over one committed/private native record boundary.

mod decoded;
mod read;
mod write;

use uqa_storage::{read_control::StorageReadControl, KeyValueBatch};

use super::{SQLiteDocumentStore, SQLiteError, SQLiteResult};
use crate::mvcc::native::{NativeRecordOwner, NativeSnapshot};

pub(super) struct NativeDocumentRead<'a> {
    snapshot: &'a NativeSnapshot,
    table: &'a str,
    owner: Option<NativeRecordOwner>,
    control: &'a StorageReadControl,
}

impl<'a> NativeDocumentRead<'a> {
    fn new(snapshot: &'a NativeSnapshot, table: &'a str) -> SQLiteResult<Self> {
        Self::with_control(snapshot, table, &snapshot.control)
    }

    fn with_control(
        snapshot: &'a NativeSnapshot,
        table: &'a str,
        control: &'a StorageReadControl,
    ) -> SQLiteResult<Self> {
        Ok(Self {
            snapshot,
            table,
            owner: snapshot.table_owner_controlled(table, control)?,
            control,
        })
    }
}

impl SQLiteDocumentStore {
    pub(super) fn read_native<R>(
        &self,
        operation: impl FnOnce(&NativeDocumentRead<'_>) -> SQLiteResult<R>,
    ) -> SQLiteResult<Option<R>> {
        self.read_native_with_control(None, operation)
    }

    pub(super) fn read_native_with_control<R>(
        &self,
        control: Option<&StorageReadControl>,
        operation: impl FnOnce(&NativeDocumentRead<'_>) -> SQLiteResult<R>,
    ) -> SQLiteResult<Option<R>> {
        let snapshot = match &self.retained {
            Some(snapshot) => Some(std::sync::Arc::clone(snapshot)),
            None => self.conn.native_snapshot()?,
        };
        snapshot
            .map(|snapshot| {
                operation(&NativeDocumentRead::with_control(
                    &snapshot,
                    &self.table,
                    control.unwrap_or(&snapshot.control),
                )?)
            })
            .transpose()
    }

    pub(super) fn write_native<R>(
        &self,
        operation: impl FnOnce(&NativeDocumentRead<'_>, &mut dyn KeyValueBatch) -> SQLiteResult<R>,
    ) -> SQLiteResult<Option<R>> {
        if self.retained.is_some() {
            return Err(SQLiteError::StorageBackend(
                "a retained document snapshot is read-only".into(),
            ));
        }
        self.conn.with_native_write(|snapshot, batch| {
            operation(&NativeDocumentRead::new(snapshot, &self.table)?, batch)
        })
    }
}
