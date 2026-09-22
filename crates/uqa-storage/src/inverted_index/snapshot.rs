//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled captures share immutable corpus payloads while each reader retains its own cancellation.

use super::{Arc, InvertedIndex, MemoryInvertedIndex, StorageBackendResult};
use crate::{read_control::StorageReadControl, ReadOnlySnapshot};

impl MemoryInvertedIndex {
    pub(super) fn controlled_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        control.check()?;
        let control = self.read_control.as_ref().unwrap_or(control);
        control.check()?;
        let memory = control.memory().reserve(size_of::<Self>())?;
        let build = |state_memory| -> StorageBackendResult<Arc<dyn InvertedIndex>> {
            let mut snapshot = self.shared_snapshot();
            snapshot.state_memory = Some(state_memory);
            snapshot.read_control = Some(control.clone());
            control.check()?;
            let snapshot: Arc<dyn InvertedIndex> = Arc::new(snapshot);
            Ok(Arc::new(
                ReadOnlySnapshot::with_retention(snapshot, memory)?
                    .with_inverted_read_control(control)?,
            ))
        };
        match &self.state_memory {
            Some(memory) => build(Arc::clone(memory)),
            None => self.state.retention.with_retained(control, build),
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
