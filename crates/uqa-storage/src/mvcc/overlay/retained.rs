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
use uqa_core::memory::BudgetedSharedMap;

pub(super) type Sources = BudgetedSharedMap<RecordKey, Option<Arc<dyn KeyValueRead + Send + Sync>>>;

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
            .records
            .get(write.key())
            .filter(|previous| previous.write.expected() != write.expected())
        {
            return Err(VersionError::WriteConflict {
                mutation: 0,
                expected: write.expected(),
                actual: previous.write.expected(),
            });
        }
        let identity = PrivateRecordRevision::allocate()?;
        let key = write.shared_key();
        let records = state
            .records
            .with_insert(key.clone(), Change { write, identity })?;
        let sources = state.sources.with_insert(key, Some(source))?;
        control.check()?;
        state.records = records;
        state.sources = sources;
        Ok(())
    }

    /// Command refresh may rebase committed preconditions. Keep only sources whose selecting bytes survive unchanged; prior snapshots/savepoints retain their own roots.
    pub(in crate::mvcc) fn inherit_retained_sources(
        &self,
        previous: &PrivateRecordChanges,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        let (previous_records, previous_sources) = {
            let previous = previous.owner.state.lock();
            if previous.sources.is_empty() {
                return Ok(());
            }
            (previous.records.clone(), previous.sources.clone())
        };
        let mut state = self.owner.state.lock();
        let mut sources = state.sources.clone();
        for (key, source) in &previous_sources {
            control.check()?;
            let Some(source) = source else { continue };
            let original = previous_records
                .get(key.bytes())
                .expect("source has selecting write");
            if state
                .records
                .get(key.bytes())
                .is_some_and(|current| current.write.value() == original.write.value())
            {
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
