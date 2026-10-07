//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered reads of the writes of a prepared commit.

use std::ops::Bound;
use std::sync::Arc;

use crate::mvcc::overlay::run::{RunCursor, RunEntry, SpilledRun};
use crate::mvcc::VersionResult;
use crate::read_control::StorageReadControl;

use super::metadata::PreparedWriteMetadata;
use super::PreparedRecordWrite;

#[cfg(test)]
mod tests;

enum Source<'a> {
    Resident(std::slice::Iter<'a, PreparedRecordWrite>),
    Spilled(RunCursor),
}

/// Visits the writes of a prepared commit in order. A spilled commit is read one block at a time, and each value is loaded only when its write is returned.
pub struct PreparedWriteCursor<'a> {
    source: Source<'a>,
}

impl<'a> PreparedWriteCursor<'a> {
    pub(super) fn resident(writes: &'a [PreparedRecordWrite]) -> Self {
        Self {
            source: Source::Resident(writes.iter()),
        }
    }

    pub(super) fn spilled(run: &Arc<SpilledRun>) -> Self {
        Self {
            source: Source::Spilled(run.cursor(Bound::Unbounded)),
        }
    }

    pub(super) fn spilled_from(run: &Arc<SpilledRun>, start: &[u8]) -> Self {
        Self {
            source: Source::Spilled(run.cursor(Bound::Included(start))),
        }
    }

    /// The next write with its value, or `None` after the last.
    pub fn next(
        &mut self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<PreparedRecordWrite>> {
        self.next_matching(None, control)
    }

    /// The next selected write, testing metadata before loading its value. Skipped
    /// spilled values are neither read nor admitted to the caller's allowance.
    pub fn next_where(
        &mut self,
        control: &StorageReadControl,
        mut select: impl FnMut(&PreparedWriteMetadata) -> VersionResult<bool>,
    ) -> VersionResult<Option<PreparedRecordWrite>> {
        match &mut self.source {
            Source::Resident(writes) => {
                for write in writes.by_ref() {
                    control.cancellation().check()?;
                    let metadata = PreparedWriteMetadata::new(
                        write.shared_key(),
                        write.expected(),
                        write.kind(),
                        write.value().map(|value| value.len() as u64),
                    );
                    if select(&metadata)? {
                        return Ok(Some(write.clone()));
                    }
                }
                control.cancellation().check()?;
                Ok(None)
            }
            Source::Spilled(cursor) => {
                while let Some(entry) = cursor.next(control)? {
                    let metadata = PreparedWriteMetadata::new(
                        entry.key.clone(),
                        entry.expected,
                        entry.kind,
                        entry.value.map(|location| location.len),
                    );
                    if select(&metadata)? {
                        return spilled_write(cursor.run(), entry, control).map(Some);
                    }
                }
                Ok(None)
            }
        }
    }

    /// Read only one record family, skipping other payloads without loading them.
    pub(in crate::mvcc) fn next_with_kind(
        &mut self,
        kind: super::RecordWriteKind,
        control: &StorageReadControl,
    ) -> VersionResult<Option<PreparedRecordWrite>> {
        self.next_matching(Some(kind), control)
    }

    fn next_matching(
        &mut self,
        kind: Option<super::RecordWriteKind>,
        control: &StorageReadControl,
    ) -> VersionResult<Option<PreparedRecordWrite>> {
        match &mut self.source {
            Source::Resident(writes) => {
                for write in writes.by_ref() {
                    control.cancellation().check()?;
                    if kind.is_none_or(|kind| write.kind() == kind) {
                        return Ok(Some(write.clone()));
                    }
                }
                control.cancellation().check()?;
                Ok(None)
            }
            Source::Spilled(cursor) => {
                while let Some(entry) = cursor.next(control)? {
                    if kind.is_none_or(|kind| entry.kind == kind) {
                        return spilled_write(cursor.run(), entry, control).map(Some);
                    }
                }
                Ok(None)
            }
        }
    }

    /// What the next write changes, without loading its value, or `None` after the last.
    pub fn next_metadata(
        &mut self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<PreparedWriteMetadata>> {
        match &mut self.source {
            Source::Resident(writes) => {
                control.cancellation().check()?;
                Ok(writes.next().map(|write| {
                    PreparedWriteMetadata::new(
                        write.shared_key(),
                        write.expected(),
                        write.kind(),
                        write.value().map(|value| value.len() as u64),
                    )
                }))
            }
            Source::Spilled(cursor) => Ok(cursor.next(control)?.map(|entry| {
                let value_len = entry.value.map(|location| location.len);
                PreparedWriteMetadata::new(entry.key, entry.expected, entry.kind, value_len)
            })),
        }
    }
}

fn spilled_write(
    run: &SpilledRun,
    entry: RunEntry,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordWrite> {
    let value = entry
        .value
        .map(|location| run.load_value(location, control))
        .transpose()?;
    Ok(PreparedRecordWrite::from_shared(entry.key, entry.expected, value).with_kind(entry.kind))
}
