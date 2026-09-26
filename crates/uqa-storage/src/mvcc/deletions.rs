//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit internal cleanup tolerates an already absent key, never a live replacement.

use super::{
    commit::RecordWriteKind, resolution::ResolutionMode, CommittedRecordSnapshot,
    PreparedRecordCommit, VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;
use uqa_core::memory::BudgetedVec;

pub(super) fn resolve(
    original: &PreparedRecordCommit,
    current: &dyn CommittedRecordSnapshot,
    mode: ResolutionMode,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    let mut writes = BudgetedVec::new(control.memory());
    writes.reserve(original.records().len())?;
    for (mutation, write) in original.records().iter().enumerate() {
        control.check()?;
        if write.kind() != RecordWriteKind::IdempotentDelete {
            writes.push(write.clone())?;
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
                mutation,
                expected: write.expected(),
                actual,
            });
        }
        writes.push(
            write
                .clone()
                .rebase(actual)
                .with_kind(mode.kind(RecordWriteKind::IdempotentDelete)),
        )?;
    }
    Ok(PreparedRecordCommit::from_unique_owned(writes, control)?
        .resolved(original, current.sequence()))
}
