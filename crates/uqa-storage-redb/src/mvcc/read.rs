//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical snapshots open and close a redb read transaction for each bounded operation.

use std::sync::Arc;

use redb::{Database, ReadableDatabase, ReadableTable};
use uqa_storage::mvcc::{
    CommitSequence, CommittedRecordSnapshot, RecordPage, RecordVersion, ScannedRecord,
    SharedRecordValue, VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

use super::{codec::value_bytes, HEADS, VERSIONS};
use crate::error::redb_error;

pub(super) struct Snapshot {
    pub(super) database: Arc<Database>,
    pub(super) sequence: CommitSequence,
}

impl CommittedRecordSnapshot for Snapshot {
    fn sequence(&self) -> CommitSequence {
        self.sequence
    }

    fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordVersion<SharedRecordValue>>> {
        control.cancellation().check()?;
        let transaction = self.database.begin_read().map_err(redb_error)?;
        let versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
        visible(&versions, key, self.sequence, control)
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
        let transaction = self.database.begin_read().map_err(redb_error)?;
        let heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
        let start = after.filter(|after| *after >= prefix).unwrap_or(prefix);
        for entry in heads.range(start..).map_err(redb_error)? {
            control.cancellation().check()?;
            let (key, _) = entry.map_err(redb_error)?;
            let key = key.value();
            if !key.starts_with(prefix) {
                break;
            }
            if after.is_some_and(|after| key <= after) {
                continue;
            }
            if let Some(version) = visible(&versions, key, self.sequence, control)? {
                result.push(ScannedRecord::copy_key(key, version, control)?)?;
                if result.len() == limit {
                    break;
                }
            }
        }
        Ok(result)
    }
}

fn visible(
    versions: &impl ReadableTable<(&'static [u8], u64), &'static [u8]>,
    key: &[u8],
    sequence: CommitSequence,
    control: &StorageReadControl,
) -> VersionResult<Option<RecordVersion<SharedRecordValue>>> {
    control.cancellation().check()?;
    let entry = versions
        .range((key, 0)..=(key, sequence.as_u64()))
        .map_err(redb_error)?
        .next_back()
        .transpose()
        .map_err(redb_error)?;
    let Some((key, bytes)) = entry else {
        return Ok(None);
    };
    let sequence = CommitSequence::from_u64(key.value().1);
    let record = RecordVersion::copy_bytes(sequence, value_bytes(bytes.value())?, control)?;
    Ok(Some(record))
}
