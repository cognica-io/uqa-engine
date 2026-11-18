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
        BackupCursorKey, BackupRecordImagePosition, BackupRecordImageVisitor, CommitSequence,
        CommittedBackupSnapshot, RecordMetadata, VersionError, VersionResult,
    },
    read_control::StorageReadControl,
};

use super::{Snapshot, EPOCH};
use crate::mvcc::{
    codec, identifiers, image_fingerprints, read, redb_error, HEADS, METADATA, VERSIONS,
};

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
        control.cancellation().check()?;
        let (transaction, _) = self.read()?;
        let heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let images = transaction
            .open_table(image_fingerprints::TABLE)
            .map_err(redb_error)?;
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        let legacy_through = codec::read_u64(&metadata, image_fingerprints::LEGACY_THROUGH)?;
        let boundary = self.records.sequence();
        let mut versions = None;
        let start = after.map_or(Bound::Unbounded, Bound::Excluded);
        for entry in heads
            .range::<&[u8]>((start, Bound::Unbounded))
            .map_err(redb_error)?
            .take(limit)
        {
            control.cancellation().check()?;
            let (key, head) = entry.map_err(redb_error)?;
            let key = key.value();
            let head = head.value();
            let image = if head <= boundary.as_u64() {
                images
                    .get((key, head))
                    .map_err(redb_error)?
                    .map(|v| (head, *v.value()))
            } else {
                images
                    .range((key, 0)..=(key, boundary.as_u64()))
                    .map_err(redb_error)?
                    .next_back()
                    .transpose()
                    .map_err(redb_error)?
                    .map(|(k, v)| (k.value().1, *v.value()))
            };
            let mut position = BackupRecordImagePosition {
                visible: None,
                fingerprint: None,
                fingerprint_bytes: 0,
            };
            position.fingerprint_bytes = if image.is_some() { 40 } else { 0 };
            let selected = image.and_then(|(revision, encoded)| {
                image_fingerprints::decode(&encoded)
                    .ok()
                    .map(|fp| (revision, fp))
            });
            if let Some((revision, fingerprint)) = selected {
                position.visible = Some(RecordMetadata {
                    revision: Some(CommitSequence::from_u64(revision)),
                    live: fingerprint.is_some(),
                });
                position.fingerprint = fingerprint;
            } else if image.is_some() || head <= boundary.as_u64() || legacy_through != 0 {
                // Older formats and unrecognized metadata use the original visibility lookup and bounded payload comparator.
                if versions.is_none() {
                    versions = Some(transaction.open_table(VERSIONS).map_err(redb_error)?);
                }
                read::visit_visible(
                    versions.as_ref().unwrap(),
                    key,
                    head,
                    boundary,
                    &mut |record| {
                        position.visible = record.map(Into::into);
                        Ok(())
                    },
                )?;
            }
            let more = visit(key, position)?;
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
