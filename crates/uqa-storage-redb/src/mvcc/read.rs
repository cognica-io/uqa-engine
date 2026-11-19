//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical snapshots open and close a redb read transaction for each bounded operation.

use std::sync::Arc;

use redb::{Database, ReadableDatabase};
use uqa_storage::mvcc::{
    BorrowedRecord, CommitSequence, CommittedRecordSnapshot, DatabaseId, RecordKeyIterator,
    RecordPage, RecordPointVisitor, RecordVersion, ScannedRecord, SharedRecordValue, VersionError,
    VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

use super::{
    codec::{validate_metadata, value_bytes},
    heads::RecordHead,
    HEADS, METADATA, VERSIONS,
};
use crate::error::redb_error;

#[cfg(test)]
thread_local! {
    pub(super) static READ_WINDOWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) struct Snapshot {
    pub(super) database: Arc<Database>,
    pub(super) identity: DatabaseId,
    pub(super) reclamation_epoch: u64,
    pub(super) lease: Arc<uqa_storage::mvcc::SnapshotLease>,
}

impl Snapshot {
    fn begin_read(&self) -> VersionResult<redb::ReadTransaction> {
        #[cfg(test)]
        READ_WINDOWS.with(|count| count.set(count.get() + 1));
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
        self.lease.sequence()
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
        let mut versions = Versions::new(&transaction);
        versions.visit(
            key,
            &RecordHead::decode(head.value())?,
            self.sequence(),
            visit,
        )?;
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

    fn visit_values(
        &self,
        keys: &mut RecordKeyIterator<'_>,
        control: &StorageReadControl,
        visit: &mut RecordPointVisitor<'_>,
    ) -> VersionResult<()> {
        control.check()?;
        let Some(first) = keys.next() else {
            return Ok(());
        };
        let mut next = Some(first?);
        let transaction = self.begin_read()?;
        let heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let mut versions = Versions::new(&transaction);
        while let Some(key) = next {
            control.check()?;
            let mut more = true;
            if let Some(head) = heads.get(&*key).map_err(redb_error)? {
                versions.visit(
                    &key,
                    &RecordHead::decode(head.value())?,
                    self.sequence(),
                    &mut |record| {
                        more = visit(&key, record)?;
                        Ok(())
                    },
                )?;
            } else {
                more = visit(&key, None)?;
            }
            control.check()?;
            if !more {
                break;
            }
            drop(key);
            next = keys.next().transpose()?;
        }
        Ok(())
    }

    fn metadata(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<uqa_storage::mvcc::RecordMetadata>> {
        control.cancellation().check()?;
        let transaction = self.begin_read()?;
        let heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let mut selected =
            super::image_fingerprints::Selection::new(&transaction, self.sequence())?;
        let record = selected.point(&heads, key)?;
        control.cancellation().check()?;
        Ok(record)
    }

    fn visit_metadata(
        &self,
        requests: &mut dyn uqa_storage::mvcc::RecordMetadataRequests,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        if !requests.advance()? {
            return control.check().map_err(Into::into);
        }
        control.check()?;
        let transaction = self.begin_read()?;
        let heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let mut selected =
            super::image_fingerprints::Selection::new(&transaction, self.sequence())?;
        loop {
            control.check()?;
            let record = selected.point(&heads, requests.key())?;
            requests.accept(record)?;
            control.check()?;
            if !requests.advance()? {
                return control.check().map_err(Into::into);
            }
        }
    }

    fn visit_keys(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut uqa_storage::mvcc::RecordKeyVisitor<'_>,
    ) -> VersionResult<()> {
        super::image_fingerprints::visit_positions::<false>(
            || self.begin_read(),
            self.sequence(),
            prefix,
            after,
            limit,
            control,
            &mut |key, position| {
                position
                    .visible
                    .map(|record| visit(key, record))
                    .transpose()
            },
        )
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
        let mut versions = Versions::new(&transaction);
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
            versions.visit(
                key,
                &RecordHead::decode(head.value())?,
                self.sequence(),
                &mut |record| {
                    if let Some(record) = record {
                        more = visit(key, record)?;
                        count += 1;
                    }
                    Ok(())
                },
            )?;
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
        let mut versions = Versions::new(&transaction);
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
            versions.visit(
                key,
                &RecordHead::decode(head.value())?,
                self.sequence(),
                &mut |record| {
                    if let Some(record) = record {
                        let version = RecordVersion::copy_bytes(
                            record.revision.expect("committed revision"),
                            record.value,
                            control,
                        )?;
                        result.push(ScannedRecord::copy_key(key, version, control)?)?;
                    }
                    Ok(())
                },
            )?;
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

type VersionTable = redb::ReadOnlyTable<(&'static [u8], u64), &'static [u8]>;

pub(super) struct Versions<'a> {
    transaction: &'a redb::ReadTransaction,
    table: Option<VersionTable>,
    bodies: super::large_values::Reader<'a>,
}

impl<'a> Versions<'a> {
    pub(super) const fn new(transaction: &'a redb::ReadTransaction) -> Self {
        Self {
            transaction,
            table: None,
            bodies: super::large_values::Reader::new(transaction),
        }
    }

    pub(super) fn visit(
        &mut self,
        key: &[u8],
        head: &RecordHead<'_>,
        sequence: CommitSequence,
        visit: &mut uqa_storage::mvcc::RecordValueVisitor<'_>,
    ) -> VersionResult<()> {
        if head.revision <= sequence.as_u64() {
            if let Some(bytes) = head.inline {
                return visit(Some(borrowed(head.revision, bytes)?));
            }
        }
        if self.table.is_none() {
            self.table = Some(self.transaction.open_table(VERSIONS).map_err(redb_error)?);
        }
        let versions = self.table.as_ref().expect("selected out-of-line versions");
        if head.revision <= sequence.as_u64() {
            let bytes = versions
                .get((key, head.revision))
                .map_err(redb_error)?
                .ok_or(VersionError::InvalidEncoding("record head has no version"))?;
            return self.bodies.with_value(bytes.value(), |raw| {
                visit(Some(borrowed(head.revision, raw)?))
            });
        }
        let entry = versions
            .range((key, 0)..=(key, sequence.as_u64()))
            .map_err(redb_error)?
            .next_back()
            .transpose()
            .map_err(redb_error)?;
        match entry {
            Some((key, bytes)) => self.bodies.with_value(bytes.value(), |raw| {
                visit(Some(borrowed(key.value().1, raw)?))
            }),
            None => visit(None),
        }
    }
}
