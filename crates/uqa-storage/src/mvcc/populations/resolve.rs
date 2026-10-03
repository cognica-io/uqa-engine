//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{reconcile::Reconciliation, structural::StructuralRecords, OwnedPopulationMutation};
use crate::mvcc::{
    commit::RecordWriteKind, resolution::ResolutionMode, CommittedRecordSnapshot,
    MergedRecordSnapshot, PreparedRecordCommit, PreparedRecordWrite, PrivateRecordChanges,
    VersionError, VersionResult, VersionedPersistence,
};
use crate::read_control::StorageReadControl;
use std::sync::Arc;
use uqa_core::memory::BudgetedVec;

pub(in crate::mvcc) fn stage(
    origins: &[PreparedRecordWrite],
    lifecycle: &[OwnedPopulationMutation],
    before: &MergedRecordSnapshot,
    after: &MergedRecordSnapshot,
    persistence: &dyn VersionedPersistence,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    Reconciliation {
        before,
        after,
        layout: persistence.diskann_population_record_layout().ok_or(
            VersionError::InvalidEncoding("provider has no DiskANN population layout"),
        )?,
        history: persistence.database_id(),
        control,
        structural: None,
    }
    .run(origins, lifecycle)
}

pub(in crate::mvcc) fn resolve(
    prepared: &PreparedRecordCommit,
    resolved: Option<PreparedRecordCommit>,
    current: &Arc<dyn CommittedRecordSnapshot>,
    persistence: &dyn VersionedPersistence,
    mode: ResolutionMode,
    control: &StorageReadControl,
) -> VersionResult<Option<PreparedRecordCommit>> {
    if prepared.populations.is_none()
        && !prepared.has_kind(RecordWriteKind::DiskANNOrigin)
        && !prepared.has_kind(RecordWriteKind::DiskANNPopulationPreview)
    {
        return Ok(resolved);
    }
    reconcile(
        resolved.as_ref().unwrap_or(prepared),
        prepared
            .populations
            .as_ref()
            .map_or(&[], |effects| &effects.operations),
        current,
        persistence,
        mode,
        control,
    )
    .map(Some)
}

fn reconcile(
    original: &PreparedRecordCommit,
    lifecycle: &[OwnedPopulationMutation],
    current: &Arc<dyn CommittedRecordSnapshot>,
    persistence: &dyn VersionedPersistence,
    mode: ResolutionMode,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    let layout =
        persistence
            .diskann_population_record_layout()
            .ok_or(VersionError::InvalidEncoding(
                "provider has no DiskANN population layout",
            ))?;
    let changes = PrivateRecordChanges::new(control.memory());
    let mut origins = BudgetedVec::new(control.memory());
    let structural = StructuralRecords::new(original, control)?;
    let mut originals = original.writes();
    let mut position = 0;
    while let Some(write) = originals.next(control)? {
        control.check()?;
        let write = &write;
        position += 1;
        let position = position - 1;
        let kind = match write.kind() {
            RecordWriteKind::DiskANNOrigin => {
                validate(current.as_ref(), position, write, control)?;
                origins.push(write.clone())?;
                mode.kind(RecordWriteKind::DiskANNOrigin)
            }
            RecordWriteKind::DiskANNPopulationPreview => {
                let header = layout.preview_header(write.key(), control)?;
                if structural
                    .get(&header, control)?
                    .is_none_or(|owner| owner.value().is_none())
                {
                    continue;
                }
                // A fenced structural copy owns its complete private metadata, including later per-document previews.
                validate(current.as_ref(), position, write, control)?;
                RecordWriteKind::Canonical
            }
            RecordWriteKind::Canonical => {
                validate(current.as_ref(), position, write, control)?;
                RecordWriteKind::Canonical
            }
            kind => kind,
        };
        changes.apply_owned(&[write.clone().with_kind(kind)], control)?;
    }
    let empty = PrivateRecordChanges::new(control.memory());
    let before = MergedRecordSnapshot::new(current.clone(), empty.snapshot()?);
    let after = MergedRecordSnapshot::new(current.clone(), changes.snapshot()?);
    let generated = Reconciliation {
        before: &before,
        after: &after,
        layout,
        history: persistence.database_id(),
        control,
        structural: Some(&structural),
    }
    .run(&origins, lifecycle)?;
    let mut generated_writes = generated.writes();
    while let Some(write) = generated_writes.next(control)? {
        changes.apply_owned(
            &[write.with_kind(mode.kind(RecordWriteKind::DiskANNPopulationPreview))],
            control,
        )?;
    }
    Ok(changes
        .prepare(control)?
        .retain_graph_effects(original, control)?
        .resolved(original, current.sequence()))
}

fn validate(
    current: &dyn CommittedRecordSnapshot,
    mutation: usize,
    write: &PreparedRecordWrite,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let actual = current
        .metadata(write.key(), control)?
        .and_then(|row| row.revision);
    if write.expected() != actual {
        return Err(VersionError::WriteConflict {
            mutation,
            expected: write.expected(),
            actual,
        });
    }
    Ok(())
}
