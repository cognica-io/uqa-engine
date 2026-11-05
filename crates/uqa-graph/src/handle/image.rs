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
    /// Transfer this captured source into an independently owned, movable image reader. The source remains retained until EOF, error, unwind or reader drop.
    pub fn into_image_reader(
        self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::image_io::ImageReader<'static>> {
        uqa_storage::image_io::ImageReader::from_owner(self, control)
    }

    /// Borrow a resumable image while preserving both the captured source and invoking cancellation owners. Preparation visits bounded native validation positions before any bytes are returned.
    pub fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::image_io::ImageReader<'a>> {
        let GraphStoreHandle::Memory(source) = self.0.source.as_ref() else {
            unreachable!("only memory graph sources can be captured");
        };
        source
            .image_reader(control)?
            .with_source_control(Some(&self.0.control), control)
    }

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

impl uqa_storage::image_io::ImageOwner for GraphImageSnapshot {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::image_io::ImageReader<'a>> {
        Self::image_reader(self, control)
    }
}

#[cfg(test)]
mod tests;
