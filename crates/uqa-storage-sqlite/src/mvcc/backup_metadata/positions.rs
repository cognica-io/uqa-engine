//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical cursor bounds keep post-capture keys inside finite read windows.

use rusqlite::OptionalExtension;
use uqa_storage::{
    mvcc::{
        BackupCursorKey, BackupRecordPositionVisitor, CommitSequence, CommittedBackupSnapshot,
        RecordMetadata, VersionError, VersionResult,
    },
    read_control::StorageReadControl,
};

use super::{history, Snapshot};
use crate::mvcc::{codec, read, PhysicalResult};

impl Snapshot {
    pub(super) fn read<T>(
        &self,
        operation: impl FnOnce(&rusqlite::Connection, u64) -> PhysicalResult<T>,
    ) -> VersionResult<T> {
        self.store.read(|connection| {
            let current = history::epoch(connection)?;
            if self.metadata_epoch().as_u64() >= current {
                return Err(VersionError::InvalidEncoding(
                    "backup metadata boundary is not sealed",
                )
                .into());
            }
            operation(connection, current)
        })
    }

    pub(super) fn identifier_ceiling(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<BackupCursorKey>> {
        control.cancellation().check()?;
        self.read(|connection, _| {
            let length: Option<i64> = connection.prepare_cached("SELECT length(namespace) FROM _uqa_mvcc_identifiers ORDER BY namespace DESC LIMIT 1")?.query_row([], |row| row.get(0)).optional()?;
            let Some(length) = length else { return Ok(None) };
            let length = crate::read_control::payload_length(length)?;
            let _payload = control.memory().reserve(length).map_err(VersionError::from)?;
            let mut statement = connection.prepare_cached("SELECT namespace FROM _uqa_mvcc_identifiers ORDER BY namespace DESC LIMIT 1")?;
            let mut rows = statement.query([])?;
            let row = rows.next()?.ok_or(VersionError::InvalidEncoding("identifier ceiling disappeared within a read"))?;
            let key = codec::bytes(row, 0)?;
            if key.len() != length { return Err(VersionError::InvalidEncoding("identifier ceiling changed within a read").into()); }
            let result = crate::read_control::copy_bytes(key, 0, control)?;
            control.cancellation().check().map_err(VersionError::from)?;
            Ok(Some(result))
        })
    }

    pub(super) fn record_positions(
        &self,
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut BackupRecordPositionVisitor<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        self.read(|connection, _| {
            read::keys(connection, b"", after, limit, control, &mut |key| {
                let _bindings = crate::read_control::reserve_bindings(control, &[key])?;
                let metadata = read::info(connection, key, self.records.sequence())?.map(|info| {
                    RecordMetadata {
                        revision: Some(CommitSequence::from_u64(info.revision)),
                        live: info.length.is_some(),
                    }
                });
                Ok(Some(visit(key, metadata)?))
            })
        })
    }
}
