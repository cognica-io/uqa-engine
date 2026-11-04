//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured graph images retain an admitted immutable source without copying entities.

use super::{Arc, GraphStoreHandle, MemoryGraphStore};
use uqa_core::memory::MemoryReservation;
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

struct RetainedGraph {
    source: Arc<GraphStoreHandle>,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

/// An export-only immutable graph generation with its original retention allowance and cancellation.
#[derive(Clone)]
pub struct GraphImageSnapshot(Arc<RetainedGraph>);

impl GraphStoreHandle {
    pub(crate) fn image_retained_bytes(&self) -> StorageBackendResult<usize> {
        let Self::Memory(source) = self else {
            return Err(StorageBackendError::Other(
                "persistent graph images belong to their storage provider".into(),
            ));
        };
        source
            .image_retained_bytes()?
            .checked_add(size_of::<Self>() - size_of::<MemoryGraphStore>())
            .ok_or_else(|| uqa_core::memory::MemoryError::SizeOverflow.into())
    }

    pub fn capture_image_with_control(
        self: &Arc<Self>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<GraphImageSnapshot> {
        control.check()?;
        let Self::Memory(source) = self.as_ref() else {
            return Err(StorageBackendError::Other(
                "persistent graph images belong to their storage provider".into(),
            ));
        };
        let mut memory = source.image_retention(control)?;
        memory
            .grow(size_of::<Self>() - size_of::<MemoryGraphStore>() + size_of::<RetainedGraph>())?;
        control.check()?;
        Ok(GraphImageSnapshot::from_admitted(
            self.clone(),
            control,
            memory,
        ))
    }
}

impl GraphImageSnapshot {
    pub(crate) const fn wrapper_bytes() -> usize {
        size_of::<RetainedGraph>()
    }

    pub(crate) fn from_admitted(
        source: Arc<GraphStoreHandle>,
        control: &StorageReadControl,
        memory: MemoryReservation,
    ) -> Self {
        Self(Arc::new(RetainedGraph {
            source,
            control: control.clone(),
            _memory: memory,
        }))
    }

    pub fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        let GraphStoreHandle::Memory(source) = self.0.source.as_ref() else {
            unreachable!("only memory graph sources can be captured");
        };
        uqa_storage::image_io::with_retained_control(output, Some(&self.0.control), |output| {
            source.write_image(output, control)
        })
    }
}

#[cfg(test)]
mod tests;
