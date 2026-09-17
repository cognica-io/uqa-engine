//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rebase evaluated storage effects for publication or the next command's private view.

use std::sync::Arc;

use super::{
    commit::RecordWriteKind, CommittedRecordSnapshot, PreparedRecordCommit, VersionResult,
    VersionedPersistence,
};
use crate::read_control::StorageReadControl;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ResolutionMode {
    Publication,
    Command,
}

impl ResolutionMode {
    pub(super) fn kind(self, private: RecordWriteKind) -> RecordWriteKind {
        match self {
            Self::Publication => RecordWriteKind::Canonical,
            Self::Command => private,
        }
    }
}

pub(super) fn has_effects(prepared: &PreparedRecordCommit) -> bool {
    prepared.graph.is_some()
        || prepared.vector.is_some()
        || prepared
            .records()
            .iter()
            .any(|write| write.kind() != RecordWriteKind::Canonical)
}

pub(super) fn resolve(
    prepared: &PreparedRecordCommit,
    base: &dyn CommittedRecordSnapshot,
    current: &Arc<dyn CommittedRecordSnapshot>,
    persistence: &dyn VersionedPersistence,
    mode: ResolutionMode,
    control: &StorageReadControl,
) -> VersionResult<Option<PreparedRecordCommit>> {
    prepared.validate_requirements(control.cancellation(), |key| {
        Ok(current
            .metadata(key, control)?
            .and_then(|record| record.revision))
    })?;
    let mut resolved = if prepared.vector.is_some() {
        Some(super::vector::resolve(
            prepared,
            base,
            current.as_ref(),
            persistence,
            mode,
            control,
        )?)
    } else {
        None
    };
    if prepared.records().iter().any(|write| {
        matches!(
            write.kind(),
            RecordWriteKind::Occurrence | RecordWriteKind::OccurrenceCache
        )
    }) {
        resolved = Some(super::occurrence::resolve(
            resolved.as_ref().unwrap_or(prepared),
            base,
            current.as_ref(),
            persistence.occurrence_record_layout(),
            mode,
            control,
        )?);
    }
    if prepared.graph.is_some() {
        let layout =
            persistence
                .graph_record_layout()
                .ok_or(super::VersionError::InvalidEncoding(
                    "provider has no graph record layout",
                ))?;
        resolved = Some(super::graph::resolve(
            resolved.as_ref().unwrap_or(prepared),
            current.clone(),
            layout,
            persistence.database_id(),
            mode,
            control,
        )?);
    }
    if prepared
        .records()
        .iter()
        .any(|write| write.kind() == RecordWriteKind::StatisticsMaintenance)
    {
        resolved = Some(super::maintenance::resolve(
            resolved.as_ref().unwrap_or(prepared),
            base,
            current.as_ref(),
            persistence.maintenance_record_layout(),
            mode,
            control,
        )?);
    }
    if prepared
        .records()
        .iter()
        .any(|write| write.kind() == RecordWriteKind::Marker)
    {
        resolved = Some(super::markers::resolve(
            resolved.as_ref().unwrap_or(prepared),
            current.as_ref(),
            mode,
            control,
        )?);
    }
    Ok(resolved)
}
