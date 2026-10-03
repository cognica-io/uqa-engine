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
        match &mut self.source {
            Source::Resident(writes) => {
                control.cancellation().check()?;
                Ok(writes.next().cloned())
            }
            Source::Spilled(cursor) => {
                let Some(entry) = cursor.next(control)? else {
                    return Ok(None);
                };
                spilled_write(cursor.run(), entry, control).map(Some)
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
