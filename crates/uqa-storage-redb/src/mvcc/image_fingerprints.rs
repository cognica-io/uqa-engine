//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-size revision metadata permits cut-qualified selection without fetching redb value pages.

use std::ops::Bound;

use redb::TableDefinition;
use uqa_storage::{
    backup::{ImageDigest, UnitFingerprint},
    mvcc::{
        BackupRecordImagePosition, CommitSequence, RecordMetadata, VersionError, VersionResult,
    },
    read_control::StorageReadControl,
};

use super::{codec, read, HEADS, METADATA, VERSIONS};
use crate::error::redb_error;

pub(super) const LEGACY_THROUGH: &str = "image_legacy_through";
pub(super) const TABLE: TableDefinition<(&[u8], u64), &[u8; 40]> =
    TableDefinition::new("uqa_mvcc_record_images_v1");

pub(super) fn initialize_boundary(
    metadata: &mut redb::Table<'_, &'static str, &'static [u8]>,
    format: u64,
) -> VersionResult<()> {
    let sequence = super::codec::read_u64(metadata, "sequence")?;
    if format < 65 {
        metadata
            .insert(LEGACY_THROUGH, sequence.to_be_bytes().as_slice())
            .map_err(redb_error)?;
    } else if super::codec::read_u64(metadata, LEGACY_THROUGH)? > sequence {
        return Err(VersionError::InvalidEncoding(
            "record image history starts beyond committed visibility",
        ));
    }
    Ok(())
}

pub(super) fn insert(
    table: &mut redb::Table<'_, (&'static [u8], u64), &'static [u8; 40]>,
    key: &[u8],
    revision: u64,
    live: Option<(usize, ImageDigest)>,
) -> VersionResult<()> {
    let mut record = [0_u8; 40];
    if let Some((length, digest)) = live {
        let encoded = u64::try_from(length)
            .ok()
            .and_then(|n| n.checked_add(8))
            .ok_or(VersionError::InvalidEncoding(
                "record image length overflow",
            ))?;
        record[..8].copy_from_slice(&encoded.to_be_bytes());
        record[8..].copy_from_slice(&digest.as_bytes());
    }
    table.insert((key, revision), &record).map_err(redb_error)?;
    Ok(())
}

pub(super) fn decode(record: &[u8; 40]) -> VersionResult<Option<UnitFingerprint>> {
    let decoded_bytes = u64::from_be_bytes(record[..8].try_into().unwrap());
    if decoded_bytes == 0 && record[8..] == [0; 32] {
        return Ok(None);
    }
    if decoded_bytes < 8 {
        return Err(VersionError::InvalidEncoding(
            "invalid revision image metadata",
        ));
    }
    Ok(Some(UnitFingerprint {
        decoded_bytes,
        digest: ImageDigest::from_bytes(record[8..].try_into().unwrap()),
    }))
}

/// Visit captured revision metadata in key order. A visitor that skips an unselected physical position does not spend an output slot.
pub(super) fn visit_positions(
    begin_read: impl FnOnce() -> VersionResult<redb::ReadTransaction>,
    boundary: CommitSequence,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    control: &StorageReadControl,
    visit: &mut impl FnMut(&[u8], BackupRecordImagePosition) -> VersionResult<Option<bool>>,
) -> VersionResult<()> {
    control.cancellation().check()?;
    if limit == 0 {
        return Ok(());
    }
    let transaction = begin_read()?;
    let heads = transaction.open_table(HEADS).map_err(redb_error)?;
    let images = transaction.open_table(TABLE).map_err(redb_error)?;
    let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
    let legacy_through = codec::read_u64(&metadata, LEGACY_THROUGH)?;
    let mut versions = None;
    let start = after
        .filter(|key| *key >= prefix)
        .map_or(Bound::Included(prefix), Bound::Excluded);
    let mut remaining = limit;
    for entry in heads
        .range::<&[u8]>((start, Bound::Unbounded))
        .map_err(redb_error)?
    {
        control.cancellation().check()?;
        let (key, head) = entry.map_err(redb_error)?;
        let key = key.value();
        if !key.starts_with(prefix) {
            break;
        }
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
            fingerprint_bytes: if image.is_some() { 40 } else { 0 },
        };
        let selected = image
            .filter(|(revision, _)| *revision != 0)
            .and_then(|(revision, encoded)| decode(&encoded).ok().map(|fp| (revision, fp)));
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
        if let Some(more) = visit(key, position)? {
            remaining -= 1;
            if !more || remaining == 0 {
                break;
            }
        }
    }
    control.cancellation().check()?;
    Ok(())
}
