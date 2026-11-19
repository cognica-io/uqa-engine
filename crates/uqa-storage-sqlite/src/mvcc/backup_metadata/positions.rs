//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical cursor bounds keep post-capture keys inside finite read windows.

use rusqlite::OptionalExtension;
use uqa_storage::{
    mvcc::{
        BackupCursorKey, BackupRecordImagePosition, BackupRecordImageVisitor, CommitSequence,
        CommittedBackupSnapshot, RecordMetadata, VersionError, VersionResult,
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
        visit: &mut BackupRecordImageVisitor<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        self.read(|connection, _| {
            read::keys(connection, b"", after, limit, control, &mut |key| {
                let _bindings = crate::read_control::reserve_bindings(control, &[key])?;
                read::with_point_info(
                    connection,
                    key,
                    self.records.sequence(),
                    read::POINT_IMAGE_METADATA_SQL,
                    |info, row| {
                        let mut fingerprint = None;
                        let visible = if let Some(info) = info {
                            if let (Some(length), Some(row)) = (info.length, row) {
                                if let rusqlite::types::ValueRef::Blob(bytes) = row.get_ref(4)? {
                                    let digest = bytes.try_into().map_err(|_| {
                                        VersionError::InvalidEncoding(
                                            "invalid record image metadata",
                                        )
                                    })?;
                                    fingerprint = Some(uqa_storage::backup::UnitFingerprint {
                                        digest: uqa_storage::backup::ImageDigest::from_bytes(
                                            digest,
                                        ),
                                        decoded_bytes: length as u64 + 8,
                                    });
                                }
                            }
                            Some(RecordMetadata {
                                revision: Some(CommitSequence::from_u64(info.revision)),
                                live: info.length.is_some(),
                            })
                        } else {
                            None
                        };
                        Ok(Some(visit(
                            key,
                            BackupRecordImagePosition {
                                visible,
                                fingerprint,
                                fingerprint_bytes: if fingerprint.is_some() { 32 } else { 0 },
                            },
                        )?))
                    },
                )
            })
        })
    }
}
