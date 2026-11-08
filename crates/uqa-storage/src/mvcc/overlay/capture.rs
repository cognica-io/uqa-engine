//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently admitted native record roots retain evaluated bytes without representation changes.

use super::{PrivateRecordChanges, PrivateRecordSnapshot, Sources};
use crate::{
    mvcc::{VersionError, VersionResult},
    read_control::StorageReadControl,
};
use uqa_core::memory::MemoryError;

impl PrivateRecordChanges {
    /// Native retained indexes own unscoped canonical records. Admit their immutable roots from maintained footprints without spilling, compacting, visiting values or retaining mutable owners. Transaction revision scopes and separately attached metadata sources belong to their existing snapshot adapters.
    pub(crate) fn capture_native_records(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<PrivateRecordSnapshot> {
        control.check()?;
        let state = self.owner.state.lock();
        if !state.sources.is_empty() || state.policy.scopes().is_some() {
            return Err(VersionError::InvalidEncoding(
                "native record capture has scoped sources",
            ));
        }
        let bytes = state
            .records
            .retained_bytes()?
            .checked_add(state.runs.retained_bytes()?)
            .and_then(|bytes| bytes.checked_add(size_of::<PrivateRecordSnapshot>()))
            .ok_or(MemoryError::SizeOverflow)?;
        let memory = control.memory().reserve(bytes)?;
        control.check()?;
        Ok(PrivateRecordSnapshot {
            records: state.records.read_root(),
            runs: state.runs.clone(),
            sources: Sources::new(control.memory()).snapshot(),
            scopes: None,
            revision: state.revision,
            _memory: memory,
        })
    }
}

#[cfg(test)]
mod tests;
