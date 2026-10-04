//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently staged immutable reads belong to their selecting private metadata replacement.

use super::{
    Change, PrivateRecordChanges, PrivateRecordRevision, PrivateRecordSnapshot, RecordKey,
};
use crate::key_value::KeyValueRead;
use crate::mvcc::{PreparedRecordWrite, VersionError, VersionResult};
use crate::read_control::StorageReadControl;
use std::sync::Arc;
use uqa_core::memory::{BudgetedSharedMap, BudgetedSharedMapSnapshot};

type Source = Option<Arc<dyn KeyValueRead + Send + Sync>>;
pub(super) type Sources = BudgetedSharedMap<RecordKey, Source>;
pub(super) type RetainedSources = BudgetedSharedMapSnapshot<RecordKey, Source>;

impl PrivateRecordChanges {
    pub(in crate::mvcc) fn apply_with_retained_source(
        &self,
        write: PreparedRecordWrite,
        source: Arc<dyn KeyValueRead + Send + Sync>,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        source.control().check()?;
        if write.value().is_none() {
            return Err(VersionError::InvalidEncoding(
                "retained metadata is deleted",
            ));
        }
        let mut state = self.owner.state.lock();
        if let Some(previous) = state
            .change(write.key(), control)?
            .filter(|previous| previous.expected() != write.expected())
        {
            return Err(VersionError::WriteConflict {
                mutation: 0,
                expected: write.expected(),
                actual: previous.expected(),
            });
        }
        let incoming = super::resident_bytes(&write);
        state.make_room(control)?;
        let identity = PrivateRecordRevision::allocate()?;
        let key = write.shared_key();
        let records = state
            .records
            .with_insert(key.clone(), Change { write, identity })?;
        let sources = state.sources.with_insert(key, Some(source))?;
        control.check()?;
        state.records = records;
        state.sources = sources;
        state.resident = state.resident.saturating_add(incoming);
        state.revision = Some(identity);
        Ok(())
    }

    /// Command refresh may rebase committed preconditions. Keep only sources whose selecting bytes survive unchanged; prior snapshots/savepoints retain their own roots.
    pub(in crate::mvcc) fn inherit_retained_sources(
        &self,
        previous: &PrivateRecordChanges,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        let (previous_records, previous_runs, previous_sources) = {
            let previous = previous.owner.state.lock();
            if previous.sources.is_empty() {
                return Ok(());
            }
            (
                previous.records.clone(),
                previous.runs.clone(),
                previous.sources.clone(),
            )
        };
        let mut state = self.owner.state.lock();
        let mut sources = state.sources.clone();
        for (key, source) in &previous_sources {
            control.check()?;
            let Some(source) = source else { continue };
            let original =
                super::tiers::lookup(&previous_records, &previous_runs, key.bytes(), control)?
                    .ok_or(VersionError::InvalidEncoding(
                        "retained source has no selecting write",
                    ))?
                    .write(control)?;
            let current = state
                .change(key.bytes(), control)?
                .map(|current| current.write(control))
                .transpose()?;
            if current.is_some_and(|current| current.value() == original.value()) {
                sources.try_insert(key.clone(), Some(source.clone()))?;
            }
        }
        control.check()?;
        state.sources = sources;
        Ok(())
    }
}

impl PrivateRecordSnapshot {
    pub(in crate::mvcc) fn retained_source(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<Arc<dyn KeyValueRead + Send + Sync>>> {
        control.check()?;
        let source = self.sources.get(key).and_then(Option::as_ref).cloned();
        if let Some(source) = &source {
            source.control().check()?;
        }
        Ok(source)
    }
}
