//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical snapshots open and close a redb read transaction for each bounded operation.

use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable};
use uqa_storage::mvcc::{
    BorrowedRecord, CommitSequence, CommittedRecordSnapshot, DatabaseId, RecordPage, RecordVersion,
    ScannedRecord, SharedRecordValue, VersionError, VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

use super::{
    codec::{validate_metadata, value_bytes},
    HEADS, METADATA, VERSIONS,
};
use crate::error::redb_error;

pub(super) struct Snapshot {
    pub(super) database: Arc<Database>,
    pub(super) identity: DatabaseId,
    pub(super) sequence: CommitSequence,
    pub(super) reclamation_epoch: u64,
    pub(super) _lease: Arc<uqa_storage::mvcc::SnapshotLease>,
}

impl Snapshot {
    fn begin_read(&self) -> VersionResult<redb::ReadTransaction> {
        let transaction = self.database.begin_read().map_err(redb_error)?;
        validate_metadata(
            &transaction.open_table(METADATA).map_err(redb_error)?,
            self.identity,
        )?;
        Ok(transaction)
    }
}

impl CommittedRecordSnapshot for Snapshot {
    fn reclamation_epoch(&self) -> Option<u64> {
        Some(self.reclamation_epoch)
    }
    fn sequence(&self) -> CommitSequence {
        self.sequence
    }

    fn visit_value(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut uqa_storage::mvcc::RecordValueVisitor<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        let transaction = self.begin_read()?;
        let heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let Some(head) = heads.get(key).map_err(redb_error)? else {
            visit(None)?;
            control.cancellation().check()?;
            return Ok(());
        };
        let versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
        visit_visible(&versions, key, head.value(), self.sequence, visit)?;
        control.cancellation().check()?;
        Ok(())
    }

    fn visit_value_bounded(
        &self,
        key: &[u8],
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut uqa_storage::mvcc::RecordValueVisitor<'_>,
    ) -> VersionResult<()> {
        self.visit_value(key, control, &mut |record| {
            if let Some(value) = record.and_then(|record| record.value) {
                control.check_value_size(value.len(), max_bytes)?;
            }
            visit(record)
        })
    }

    fn visit_prefix(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut uqa_storage::mvcc::RecordScanVisitor<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        if limit == 0 {
            return Ok(());
        }
        let transaction = self.begin_read()?;
        let heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
        let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
        let mut count = 0;
        for entry in heads.range(start..).map_err(redb_error)? {
            control.cancellation().check()?;
            let (key, head) = entry.map_err(redb_error)?;
            let key = key.value();
            if !key.starts_with(prefix) {
                break;
            }
            if after.is_some_and(|after| key <= after) {
                continue;
            }
            let mut more = true;
            visit_visible(&versions, key, head.value(), self.sequence, &mut |record| {
                if let Some(record) = record {
                    more = visit(key, record)?;
                    count += 1;
                }
                Ok(())
            })?;
            control.cancellation().check()?;
            if !more || count == limit {
                break;
            }
        }
        Ok(())
    }

    fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordVersion<SharedRecordValue>>> {
        let mut found = None;
        self.visit_value(key, control, &mut |record| {
            found = record
                .map(|record| {
                    RecordVersion::copy_bytes(
                        record.revision.expect("committed revision"),
                        record.value,
                        control,
                    )
                })
                .transpose()?;
            Ok(())
        })?;
        Ok(found)
    }

    fn scan(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<RecordPage> {
        control.cancellation().check()?;
        let mut result = RecordPage::new(control.memory());
        if limit == 0 {
            return Ok(result);
        }
        let transaction = self.begin_read()?;
        let heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
        let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
        for entry in heads.range(start..).map_err(redb_error)? {
            control.cancellation().check()?;
            let (key, head) = entry.map_err(redb_error)?;
            let key = key.value();
            if !key.starts_with(prefix) {
                break;
            }
            if after.is_some_and(|after| key <= after) {
                continue;
            }
            visit_visible(&versions, key, head.value(), self.sequence, &mut |record| {
                if let Some(record) = record {
                    let version = RecordVersion::copy_bytes(
                        record.revision.expect("committed revision"),
                        record.value,
                        control,
                    )?;
                    result.push(ScannedRecord::copy_key(key, version, control)?)?;
                }
                Ok(())
            })?;
            if result.len() == limit {
                break;
            }
        }
        Ok(result)
    }
}

fn borrowed(sequence: u64, bytes: &[u8]) -> VersionResult<BorrowedRecord<'_>> {
    if sequence == 0 {
        return Err(VersionError::InvalidEncoding("zero record revision"));
    }
    Ok(BorrowedRecord {
        revision: Some(CommitSequence::from_u64(sequence)),
        value: value_bytes(bytes)?,
    })
}

fn visit_visible(
    versions: &impl ReadableTable<(&'static [u8], u64), &'static [u8]>,
    key: &[u8],
    head: u64,
    sequence: CommitSequence,
    visit: &mut uqa_storage::mvcc::RecordValueVisitor<'_>,
) -> VersionResult<()> {
    if head <= sequence.as_u64() {
        let bytes = versions
            .get((key, head))
            .map_err(redb_error)?
            .ok_or(VersionError::InvalidEncoding("record head has no version"))?;
        return visit(Some(borrowed(head, bytes.value())?));
    }
    let entry = versions
        .range((key, 0)..=(key, sequence.as_u64()))
        .map_err(redb_error)?
        .next_back()
        .transpose()
        .map_err(redb_error)?;
    visit(
        entry
            .as_ref()
            .map(|(key, bytes)| borrowed(key.value().1, bytes.value()))
            .transpose()?,
    )
}
