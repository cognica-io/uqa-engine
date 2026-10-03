//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable marker revisions coordinate structural changes without conflicting independent data writes.

use super::{
    commit::{PreparedWritesBuilder, RecordWriteKind},
    resolution::ResolutionMode,
    CommittedRecordSnapshot, PreparedRecordCommit, RecordVersion, VersionError, VersionResult,
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
    while let Some(write) = originals.next(control)? {
        if write.kind() != RecordWriteKind::Marker {
            writes.push(write, control)?;
            continue;
        }
        let value = write.value().ok_or(VersionError::InvalidEncoding(
            "revision markers must have an immutable value",
        ))?;
        let record = current.get(write.key(), control)?;
        if record
            .as_ref()
            .and_then(RecordVersion::value)
            .is_some_and(|stored| &***stored != value)
        {
            return Err(VersionError::InvalidEncoding(
                "revision marker payload changed",
            ));
        }
        writes.push(
            write
                .rebase(record.as_ref().map(RecordVersion::sequence))
                .with_kind(mode.kind(RecordWriteKind::Marker)),
            control,
        )?;
    }
    Ok(writes
        .finish(control)?
        .resolved(original, current.sequence()))
}
