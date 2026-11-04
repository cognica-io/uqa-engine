//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture the absolute memory counter position with the data cut it accompanies.

use super::{CatalogOidCounter, FIRST_NORMAL_OBJECT_ID};
use uqa_core::memory::Budgeted;
use uqa_storage::{
    memory_capture::CapturedMemoryValue, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};

pub type CapturedCatalogOidPosition = CapturedMemoryValue<Option<u64>>;

impl CatalogOidCounter {
    /// Preserve the memory counter while the callback registers its matching data cut. The callback must not allocate catalog OIDs or reenter this counter. Persistent providers capture their own durable allocator namespace instead.
    pub fn capture_with<T>(
        &self,
        control: &StorageReadControl,
        capture: impl FnOnce() -> StorageBackendResult<T>,
    ) -> StorageBackendResult<(T, CapturedCatalogOidPosition)> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<Option<u64>>())?;
        let position = loop {
            control.check()?;
            if let Some(position) = self
                .position
                .try_lock_for(std::time::Duration::from_millis(10))
            {
                break position;
            }
        };
        let captured = CapturedMemoryValue::new(Budgeted::new(*position, memory), control)?;
        let selected = capture()?;
        control.check()?;
        drop(position);
        Ok((selected, captured))
    }

    /// Construct an independent fresh counter, preserving uninitialized state, complete wrap history and exhaustion without changing an existing live counter.
    pub fn from_position(position: Option<u64>) -> StorageBackendResult<Self> {
        if position.is_some_and(|value| value < u64::from(FIRST_NORMAL_OBJECT_ID)) {
            return Err(StorageBackendError::Other(
                "catalog OID counter position precedes the first normal object ID".into(),
            ));
        }
        Ok(Self {
            position: parking_lot::Mutex::new(position),
        })
    }
}

#[cfg(test)]
mod tests;
