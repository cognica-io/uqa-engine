//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A finite byte range belongs to the same committed version as its length and revision.

use super::{BorrowedRecord, CommitSequence, CommittedRecordSnapshot, StorageReadControl};
use crate::mvcc::{VersionError, VersionResult};

/// Result of copying one committed value range into caller-owned storage. `None` length means a tombstone; `Some(0)` means a live empty value. An absent key has no result at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordValueChunk {
    revision: CommitSequence,
    total_bytes: Option<usize>,
    copied: usize,
}

impl RecordValueChunk {
    /// Validate a provider's selected range before copying bytes. Offset equal to length is EOF; an offset beyond it is invalid even for an empty destination. Tombstones permit only offset zero.
    pub fn for_range(
        revision: CommitSequence,
        total_bytes: Option<usize>,
        offset: usize,
        capacity: usize,
    ) -> VersionResult<Self> {
        let remaining =
            total_bytes
                .unwrap_or(0)
                .checked_sub(offset)
                .ok_or(VersionError::InvalidEncoding(
                    "record chunk offset exceeds its value",
                ))?;
        Ok(Self {
            revision,
            total_bytes,
            copied: remaining.min(capacity),
        })
    }

    pub const fn revision(self) -> CommitSequence {
        self.revision
    }
    pub const fn total_bytes(self) -> Option<usize> {
        self.total_bytes
    }
    pub const fn copied(self) -> usize {
        self.copied
    }

    /// Copy a borrowed committed record without retaining its provider read window. The caller owns and accounts for the destination buffer; this operation allocates no payload storage.
    pub fn copy_from(
        record: BorrowedRecord<'_>,
        offset: usize,
        output: &mut [u8],
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        control.cancellation().check()?;
        let revision = record.revision.ok_or(VersionError::InvalidEncoding(
            "committed chunk has no revision",
        ))?;
        let result = Self::for_range(
            revision,
            record.value.map(<[u8]>::len),
            offset,
            output.len(),
        )?;
        if let Some(value) = record.value {
            for (index, part) in output[..result.copied].chunks_mut(16 * 1024).enumerate() {
                control.cancellation().check()?;
                let start = offset + index * 16 * 1024;
                part.copy_from_slice(&value[start..start + part.len()]);
            }
        }
        control.cancellation().check()?;
        Ok(result)
    }
}

pub(super) fn read<S: CommittedRecordSnapshot + ?Sized>(
    snapshot: &S,
    key: &[u8],
    offset: usize,
    output: &mut [u8],
    control: &StorageReadControl,
) -> VersionResult<Option<RecordValueChunk>> {
    control.cancellation().check()?;
    let mut result = None;
    snapshot.visit_value(key, control, &mut |record| {
        result = record
            .map(|record| RecordValueChunk::copy_from(record, offset, output, control))
            .transpose()?;
        Ok(())
    })?;
    control.cancellation().check()?;
    Ok(result)
}

#[cfg(test)]
mod tests;
