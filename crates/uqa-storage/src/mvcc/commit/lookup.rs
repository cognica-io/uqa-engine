//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Point lookups into the writes of a prepared commit.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::memory::{MemoryError, MemoryReservation};

use crate::mvcc::overlay::run::{RunCacheReader, SpilledRun};
use crate::mvcc::VersionResult;
use crate::read_control::StorageReadControl;

use super::writes::PreparedWrites;
use super::{PreparedRecordCommit, PreparedRecordWrite};

enum Source<'a> {
    Resident {
        writes: BTreeMap<&'a [u8], &'a PreparedRecordWrite>,
        _memory: MemoryReservation,
    },
    Spilled {
        run: &'a Arc<SpilledRun>,
        _reader: RunCacheReader,
    },
}

/// Finds the write of a key in a prepared commit: through an ordered index of a batch in memory, or through the key filter and block index of a spilled batch.
pub(in crate::mvcc) struct PreparedLookup<'a> {
    source: Source<'a>,
}

impl<'a> PreparedLookup<'a> {
    pub(in crate::mvcc) fn new(
        prepared: &'a PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let source = match &prepared.writes {
            PreparedWrites::Resident(writes) => {
                let size = writes
                    .len()
                    .checked_mul(size_of::<(&[u8], &PreparedRecordWrite)>())
                    .ok_or(MemoryError::SizeOverflow)?;
                // Charge logical entries; allocator-specific tree-node bookkeeping follows the other record maps.
                let memory = control.memory().reserve(size)?;
                let mut index = BTreeMap::new();
                for write in writes.iter() {
                    control.cancellation().check()?;
                    index.insert(write.key(), write);
                }
                Source::Resident {
                    writes: index,
                    _memory: memory,
                }
            }
            PreparedWrites::Spilled(run) => Source::Spilled {
                run,
                _reader: run.cache_reader(),
            },
        };
        Ok(Self { source })
    }

    /// The write of `key`, with its value.
    pub(in crate::mvcc) fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<PreparedRecordWrite>> {
        match &self.source {
            Source::Resident { writes, .. } => Ok(writes.get(key).map(|write| (*write).clone())),
            Source::Spilled { run, .. } => run
                .get(key, control)?
                .map(|entry| {
                    let value = entry
                        .value
                        .map(|location| run.load_value(location, control))
                        .transpose()?;
                    Ok(
                        PreparedRecordWrite::from_shared(entry.key, entry.expected, value)
                            .with_kind(entry.kind),
                    )
                })
                .transpose(),
        }
    }

    /// Visit the writes whose keys start with `prefix` in key order, until `visit` returns false; the batch must be ordered by key.
    pub(in crate::mvcc) fn visit_prefix(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
        visit: &mut dyn FnMut(&PreparedRecordWrite) -> VersionResult<bool>,
    ) -> VersionResult<()> {
        match &self.source {
            Source::Resident { writes, .. } => {
                for (key, write) in writes.range::<[u8], _>((
                    std::ops::Bound::Included(prefix),
                    std::ops::Bound::Unbounded,
                )) {
                    control.cancellation().check()?;
                    if !key.starts_with(prefix) || !visit(write)? {
                        break;
                    }
                }
            }
            Source::Spilled { run, .. } => {
                let mut cursor = run.cursor(std::ops::Bound::Included(prefix));
                while let Some(entry) = cursor.next(control)? {
                    if !entry.key.bytes().starts_with(prefix) {
                        break;
                    }
                    let value = entry
                        .value
                        .map(|location| run.load_value(location, control))
                        .transpose()?;
                    let write = PreparedRecordWrite::from_shared(entry.key, entry.expected, value)
                        .with_kind(entry.kind);
                    if !visit(&write)? {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    pub(in crate::mvcc) fn contains(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<bool> {
        match &self.source {
            Source::Resident { writes, .. } => Ok(writes.contains_key(key)),
            Source::Spilled { run, .. } => Ok(run.get(key, control)?.is_some()),
        }
    }
}
