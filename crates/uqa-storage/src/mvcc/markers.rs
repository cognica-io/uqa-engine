//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable marker revisions coordinate structural changes without conflicting independent data writes.

use uqa_core::memory::BudgetedVec;

use super::{
    commit::RecordWriteKind, CommittedRecordSnapshot, PreparedRecordCommit, RecordVersion,
    VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;

pub(super) fn resolve(
    original: &PreparedRecordCommit,
    current: &dyn CommittedRecordSnapshot,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    let mut writes = BudgetedVec::new(control.memory());
    writes.reserve(original.records().len())?;
    for write in original.records() {
        control.cancellation().check()?;
        if write.kind() != RecordWriteKind::Marker {
            writes.push(write.clone())?;
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
                .clone()
                .rebase(record.as_ref().map(RecordVersion::sequence))
                .with_kind(RecordWriteKind::Canonical),
        )?;
    }
    Ok(PreparedRecordCommit::from_unique_owned(writes, control)?
        .resolved(original, current.sequence()))
}
