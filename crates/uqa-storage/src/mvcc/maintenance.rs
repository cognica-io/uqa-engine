//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Merge evaluated statistics-maintenance changes without recollecting statistics or replaying writes.

use uqa_core::memory::BudgetedVec;

use super::{
    commit::RecordWriteKind, resolution::ResolutionMode, CommittedRecordSnapshot,
    PreparedRecordCommit, VersionError, VersionResult,
};
use crate::{read_control::StorageReadControl, statistics_maintenance::StatisticsMaintenance};

pub trait MaintenanceRecordLayout: Send + Sync {
    fn decode(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<StatisticsMaintenance>;
    fn encode(
        &self,
        key: &[u8],
        template: &[u8],
        state: &StatisticsMaintenance,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;
}

pub(super) fn resolve(
    original: &PreparedRecordCommit,
    base: &dyn CommittedRecordSnapshot,
    current: &dyn CommittedRecordSnapshot,
    layout: &dyn MaintenanceRecordLayout,
    mode: ResolutionMode,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    let mut writes = BudgetedVec::new(control.memory());
    writes.reserve(original.records().len())?;
    for write in original.records() {
        control.cancellation().check()?;
        if write.kind() != RecordWriteKind::StatisticsMaintenance {
            writes.push(write.clone())?;
            continue;
        }
        let value = write.value().ok_or(VersionError::InvalidEncoding(
            "statistics maintenance cannot stage a typed deletion",
        ))?;
        let prior = base.get(write.key(), control)?;
        let latest = current.get(write.key(), control)?;
        let decode = |record: Option<&super::RecordVersion<super::SharedRecordValue>>| {
            record
                .and_then(|record| record.value())
                .map(|value| layout.decode(write.key(), value, control))
                .transpose()
                .map(Option::unwrap_or_default)
        };
        let before = decode(prior.as_ref())?;
        let after = layout.decode(write.key(), value, control)?;
        let latest_state = decode(latest.as_ref())?;
        let deleted = latest
            .as_ref()
            .is_none_or(|record| record.value().is_none())
            && prior.as_ref().map(super::RecordVersion::sequence)
                != latest.as_ref().map(super::RecordVersion::sequence);
        let merged = if deleted {
            None
        } else {
            StatisticsMaintenance::merge(&before, &after, &latest_state)?
        };
        let Some(merged) = merged else {
            // Preserve the original conditional replacement for object replacement or competing resets. The normal validator then reports a conflict rather than resurrecting an old relation's state.
            writes.push(write.clone().with_kind(RecordWriteKind::Canonical))?;
            continue;
        };
        writes.push(
            write
                .clone()
                .with_value(layout.encode(write.key(), value, &merged, control)?)
                .rebase(latest.as_ref().map(super::RecordVersion::sequence))
                .with_kind(mode.kind(RecordWriteKind::StatisticsMaintenance)),
        )?;
    }
    Ok(PreparedRecordCommit::from_unique_owned(writes, control)?
        .resolved(original, current.sequence()))
}
