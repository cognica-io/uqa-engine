//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scan positions include new keys without extending the captured record domain.

use std::ops::Bound;

use redb::{ReadableDatabase, ReadableTable};
use uqa_storage::{
    mvcc::{
        BackupCursorKey, BackupRecordPositionVisitor, CommittedBackupSnapshot, VersionError,
        VersionResult,
    },
    read_control::StorageReadControl,
};

use super::{Snapshot, EPOCH};
use crate::mvcc::{codec, identifiers, read, redb_error, HEADS, METADATA, VERSIONS};

impl Snapshot {
    pub(super) fn read(&self) -> VersionResult<(redb::ReadTransaction, u64)> {
        let transaction = self.database.begin_read().map_err(redb_error)?;
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        codec::validate_metadata(&metadata, self.identity)?;
        let epoch = codec::read_u64(&metadata, EPOCH)?;
        if self.metadata_epoch().as_u64() >= epoch {
            return Err(VersionError::InvalidEncoding(
                "backup metadata boundary is not sealed",
            ));
        }
        drop(metadata);
        Ok((transaction, epoch))
    }

    pub(super) fn record_ceiling(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<BackupCursorKey>> {
        control.cancellation().check()?;
        let (transaction, _) = self.read()?;
        let heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let result = heads
            .last()
            .map_err(redb_error)?
            .map(|(key, _)| copy_key(key.value(), control))
            .transpose()?;
        control.cancellation().check()?;
        Ok(result)
    }

    pub(super) fn identifier_ceiling(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<BackupCursorKey>> {
        control.cancellation().check()?;
        let (transaction, _) = self.read()?;
        let identifiers = transaction
            .open_table(identifiers::TABLE)
            .map_err(redb_error)?;
        let result = identifiers
            .last()
            .map_err(redb_error)?
            .map(|(key, _)| copy_key(key.value(), control))
            .transpose()?;
        control.cancellation().check()?;
        Ok(result)
    }

    pub(super) fn record_positions(
        &self,
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut BackupRecordPositionVisitor<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        let (transaction, _) = self.read()?;
        let heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
        let start = after.map_or(Bound::Unbounded, Bound::Excluded);
        for entry in heads
            .range::<&[u8]>((start, Bound::Unbounded))
            .map_err(redb_error)?
            .take(limit)
        {
            control.cancellation().check()?;
            let (key, head) = entry.map_err(redb_error)?;
            let mut more = true;
            read::visit_visible(
                &versions,
                key.value(),
                head.value(),
                self.records.sequence(),
                &mut |record| {
                    more = visit(key.value(), record.map(Into::into))?;
                    Ok(())
                },
            )?;
            if !more {
                break;
            }
        }
        control.cancellation().check()?;
        Ok(())
    }
}

fn copy_key(key: &[u8], control: &StorageReadControl) -> VersionResult<BackupCursorKey> {
    let mut result = BackupCursorKey::new(control.memory());
    result.extend_from_slice(key)?;
    Ok(result)
}
