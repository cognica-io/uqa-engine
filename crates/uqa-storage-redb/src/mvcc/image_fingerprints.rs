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

use super::{codec, heads::RecordHead, read, HEADS, METADATA};
use crate::error::redb_error;

/// A displaced uncertified revision must shadow older known digests at its own position.
pub(super) const UNCERTIFIED_HISTORY: [u8; 40] = {
    let mut marker = [0; 40];
    marker[8] = 1;
    marker
};

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

pub(super) fn large_certificate(encoded: &[u8; 40]) -> Option<UnitFingerprint> {
    decode(encoded)
        .ok()?
        .filter(|image| image.decoded_bytes > 8 + super::heads::INLINE_LIMIT as u64)
}

type ImageHistory = redb::ReadOnlyTable<(&'static [u8], u64), &'static [u8; 40]>;

pub(super) struct Selection<'a> {
    transaction: &'a redb::ReadTransaction,
    boundary: CommitSequence,
    legacy_through: u64,
    images: Option<ImageHistory>,
    versions: read::Versions<'a>,
}

impl<'a> Selection<'a> {
    pub(super) fn new(
        transaction: &'a redb::ReadTransaction,
        boundary: CommitSequence,
    ) -> VersionResult<Self> {
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        Ok(Self {
            transaction,
            boundary,
            legacy_through: codec::read_u64(&metadata, LEGACY_THROUGH)?,
            images: None,
            versions: read::Versions::new(transaction),
        })
    }

    pub(super) fn point(
        &mut self,
        heads: &redb::ReadOnlyTable<&'static [u8], &'static [u8]>,
        key: &[u8],
    ) -> VersionResult<Option<RecordMetadata>> {
        let Some(head) = heads.get(key).map_err(redb_error)? else {
            return Ok(None);
        };
        Ok(self.position::<false>(key, head.value())?.visible)
    }

    fn images(&mut self) -> VersionResult<&ImageHistory> {
        if self.images.is_none() {
            self.images = Some(self.transaction.open_table(TABLE).map_err(redb_error)?);
        }
        Ok(self.images.as_ref().expect("opened image table"))
    }

    fn position<const WITH_FINGERPRINT: bool>(
        &mut self,
        key: &[u8],
        raw: &[u8],
    ) -> VersionResult<BackupRecordImagePosition> {
        let head = RecordHead::decode(raw)?;
        let head_revision = head.revision;
        if head.indexed_live && head_revision <= self.boundary.as_u64() {
            let (fingerprint, bytes) = if WITH_FINGERPRINT {
                self.images()?
                    .get((key, head_revision))
                    .map_err(redb_error)?
                    .map_or((None, 0), |value| (large_certificate(value.value()), 40))
            } else {
                (None, 0)
            };
            return Ok(BackupRecordImagePosition {
                visible: Some(RecordMetadata {
                    revision: Some(CommitSequence::from_u64(head_revision)),
                    live: true,
                }),
                fingerprint,
                fingerprint_bytes: bytes,
            });
        }
        let image = if head_revision <= self.boundary.as_u64() {
            Some((head_revision, head.certificate, head.certificate_bytes))
        } else {
            let sequence = self.boundary.as_u64();
            self.images()?
                .range((key, 0)..=(key, sequence))
                .map_err(redb_error)?
                .next_back()
                .transpose()
                .map_err(redb_error)?
                .map(|(k, v)| {
                    (
                        k.value().1,
                        (*v.value() != UNCERTIFIED_HISTORY).then(|| *v.value()),
                        40,
                    )
                })
        };
        let mut position = BackupRecordImagePosition {
            visible: None,
            fingerprint: None,
            fingerprint_bytes: image.map_or(0, |(_, _, bytes)| bytes),
        };
        if let Some((revision, encoded, _)) = image.filter(|(revision, _, _)| *revision != 0) {
            let selected = if revision == head_revision && head.live_empty() {
                // Ordinary key-only scans need only the explicit live-empty tag.
                Some((
                    true,
                    WITH_FINGERPRINT.then(|| super::heads::empty_fingerprint(revision)),
                ))
            } else {
                encoded
                    .and_then(|value| decode(&value).ok())
                    .map(|fingerprint| (fingerprint.is_some(), fingerprint))
            };
            if let Some((live, fingerprint)) = selected {
                position.visible = Some(RecordMetadata {
                    revision: Some(CommitSequence::from_u64(revision)),
                    live,
                });
                position.fingerprint = fingerprint;
            }
        }
        if position.visible.is_none()
            && (image.is_some()
                || head_revision <= self.boundary.as_u64()
                || self.legacy_through != 0)
        {
            // Older formats and unrecognized metadata use the original visibility lookup and bounded payload comparator.
            self.versions
                .visit(key, &head, self.boundary, &mut |record| {
                    position.visible = record.map(Into::into);
                    Ok(())
                })?;
        }
        Ok(position)
    }
}

/// Visit captured revision metadata in key order. A visitor that skips an unselected physical position does not spend an output slot.
pub(super) fn visit_positions<const WITH_FINGERPRINT: bool>(
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
    let mut selected = Selection::new(&transaction, boundary)?;
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
        let position = selected.position::<WITH_FINGERPRINT>(key, head.value())?;
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
