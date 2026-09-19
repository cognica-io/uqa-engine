//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reclaim predecessor histories under common snapshot admission and one physical writer.

use super::{codec, physical_writer, RedbRecordStore, HEADS, METADATA, VERSIONS};
use crate::error::redb_error;
use redb::ReadableTable;
use uqa_storage::mvcc::{CommitSequence, ReclamationHorizon, VersionError, VersionResult};
use uqa_storage::read_control::StorageReadControl;

pub(super) fn reclaim(
    store: &RedbRecordStore,
    oldest: Option<CommitSequence>,
    control: &StorageReadControl,
) -> VersionResult<u64> {
    control.cancellation().check()?;
    let transaction = physical_writer(&store.database)?;
    let mut removed = 0_u64;
    {
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        codec::validate_metadata(&metadata, store.identity)?;
        let current = CommitSequence::from_u64(codec::read_u64(&metadata, "sequence")?);
        let horizon = ReclamationHorizon::new(current, oldest)?;
        let heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let mut versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
        for entry in heads.iter().map_err(redb_error)? {
            control.cancellation().check()?;
            let (key, _) = entry.map_err(redb_error)?;
            let key = key.value();
            let cutoff = versions
                .range((key, 0)..=(key, horizon.sequence().as_u64()))
                .map_err(redb_error)?
                .next_back()
                .transpose()
                .map_err(redb_error)?
                .map(|(key, _)| key.value().1);
            let Some(cutoff) = cutoff else {
                continue;
            };
            let cutoff = horizon.anchor(CommitSequence::from_u64(cutoff))?.as_u64();
            loop {
                control.cancellation().check()?;
                let obsolete = versions
                    .range((key, 0)..(key, cutoff))
                    .map_err(redb_error)?
                    .next()
                    .transpose()
                    .map_err(redb_error)?
                    .map(|(key, _)| key.value().1);
                let Some(obsolete) = obsolete else {
                    break;
                };
                versions.remove((key, obsolete)).map_err(redb_error)?;
                removed = removed.checked_add(1).ok_or(VersionError::InvalidEncoding(
                    "reclaimed version count overflow",
                ))?;
            }
        }
    }
    control.cancellation().check()?;
    transaction.commit().map_err(redb_error)?;
    Ok(removed)
}
