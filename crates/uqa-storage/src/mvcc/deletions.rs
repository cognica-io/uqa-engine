//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit internal cleanup tolerates an already absent key, never a live replacement.

use super::{
    commit::{PreparedWritesBuilder, RecordWriteKind},
    resolution::ResolutionMode,
    CommittedRecordSnapshot, PreparedRecordCommit, VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;

pub(super) fn resolve(
    original: &PreparedRecordCommit,
    current: &dyn CommittedRecordSnapshot,
    mode: ResolutionMode,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    let mut writes = PreparedWritesBuilder::like(original, control)?;
    let mut originals = original.writes();
    let mut mutation = 0;
    while let Some(write) = originals.next(control)? {
        control.check()?;
        mutation += 1;
        if write.kind() != RecordWriteKind::IdempotentDelete {
            writes.push(write, control)?;
            continue;
        }
        if write.value().is_some() {
            return Err(VersionError::InvalidEncoding(
                "idempotent deletion cannot contain a value",
            ));
        }
        let record = current.metadata(write.key(), control)?;
        let actual = record.and_then(|record| record.revision);
        if record.is_some_and(|record| record.live) && write.expected() != actual {
            return Err(VersionError::WriteConflict {
                mutation: mutation - 1,
                expected: write.expected(),
                actual,
            });
        }
        writes.push(
            write
                .rebase(actual)
                .with_kind(mode.kind(RecordWriteKind::IdempotentDelete)),
            control,
        )?;
    }
    Ok(writes
        .finish(control)?
        .resolved(original, current.sequence()))
}
