//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    reconcile::Reconciliation, structural::StructuralRecords, DiskANNPopulationRecordLayout,
    OwnedPopulationMutation,
};
use crate::mvcc::{
    commit::{PreparedWritesBuilder, RecordWriteKind},
    resolution::ResolutionMode,
    CommittedRecordSnapshot, DatabaseId, MergedRecordSnapshot, PreparedRecordCommit,
    PreparedRecordWrite, PrivateRecordChanges, VersionError, VersionResult, VersionedPersistence,
};
use crate::read_control::StorageReadControl;
use std::sync::Arc;

#[cfg(test)]
mod tests;

pub(in crate::mvcc) fn stage(
    origins: &PreparedRecordCommit,
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
        persistence
            .diskann_population_record_layout()
            .ok_or(VersionError::InvalidEncoding(
                "provider has no DiskANN population layout",
            ))?,
        persistence.database_id(),
        mode,
        control,
    )
    .map(Some)
}

fn reconcile(
    original: &PreparedRecordCommit,
    lifecycle: &[OwnedPopulationMutation],
    current: &Arc<dyn CommittedRecordSnapshot>,
    layout: &dyn DiskANNPopulationRecordLayout,
    history: DatabaseId,
    mode: ResolutionMode,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    let mut changes = PreparedWritesBuilder::like(original, control)?;
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
        changes.push(write.clone().with_kind(kind), control)?;
    }
    drop(originals);
    let changes = changes.finish_changes(None, control)?;
    let empty = PrivateRecordChanges::new(control.memory());
    let before = MergedRecordSnapshot::new(current.clone(), empty.snapshot()?);
    let after = MergedRecordSnapshot::new(current.clone(), changes.snapshot()?);
    let generated = Reconciliation {
        before: &before,
        after: &after,
        layout,
        history,
        control,
        structural: Some(&structural),
    }
    .run(original, lifecycle)?;
    drop(after);
    drop(before);
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
