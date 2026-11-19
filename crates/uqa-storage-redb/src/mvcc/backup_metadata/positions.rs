//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scan positions include new keys without extending the captured record domain.

use redb::{ReadableDatabase, ReadableTable};
use uqa_storage::{
    mvcc::{
        BackupCursorKey, BackupRecordImageVisitor, CommittedBackupSnapshot, VersionError,
        VersionResult,
    },
    read_control::StorageReadControl,
};

use super::{Snapshot, EPOCH};
use crate::mvcc::{codec, identifiers, image_fingerprints, redb_error, HEADS, METADATA};

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
        visit: &mut BackupRecordImageVisitor<'_>,
    ) -> VersionResult<()> {
        image_fingerprints::visit_positions::<true>(
            || self.read().map(|(transaction, _)| transaction),
            self.records.sequence(),
            b"",
            after,
            limit,
            control,
            &mut |key, position| visit(key, position).map(Some),
        )
    }
}

fn copy_key(key: &[u8], control: &StorageReadControl) -> VersionResult<BackupCursorKey> {
    let mut result = BackupCursorKey::new(control.memory());
    result.extend_from_slice(key)?;
    Ok(result)
}
