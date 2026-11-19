//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native path images retain their original materialization and capture controls.

use super::{CapturedPathIndex, MemoryPaths, PathIndex, PathStorage};
use std::sync::Arc;
use uqa_core::memory::{MemoryError, MemoryReservation};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

struct RetainedPathIndex {
    paths: Arc<MemoryPaths>,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

#[derive(Clone)]
pub struct PathIndexImageSnapshot(Arc<RetainedPathIndex>);

impl PathIndexImageSnapshot {
    /// Transfer the selected generation into a movable image reader independent of the source registry and Engine. Both capture and invoking cancellation remain active until completion or retirement.
    pub fn into_image_reader(
        self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::image_io::ImageReader<'static>> {
        uqa_storage::image_io::ImageReader::from_owner(self, control)
    }

    /// Borrow bounded output from the original admitted native materialization.
    pub fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::image_io::ImageReader<'a>> {
        super::image::reader(self.get()?, control)?
            .with_source_control(Some(&self.0.control), control)
    }

    pub fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        let image = self.get()?;
        uqa_storage::image_io::with_retained_control(output, Some(&self.0.control), |output| {
            super::image::write(image, output, control)
        })
    }

    /// Borrow exact ordered sequences and evaluated pairs under the original capture's cancellation scope.
    pub fn get(&self) -> StorageBackendResult<&CapturedPathIndex> {
        self.0.control.check()?;
        Ok(&self.0.paths.image)
    }

    pub(super) fn wrapper_bytes() -> usize {
        size_of::<RetainedPathIndex>()
    }

    pub(super) fn from_admitted(
        paths: Arc<MemoryPaths>,
        control: &StorageReadControl,
        memory: MemoryReservation,
    ) -> Self {
        Self(Arc::new(RetainedPathIndex {
            paths,
            control: control.clone(),
            _memory: memory,
        }))
    }
}

impl uqa_storage::image_io::ImageOwner for PathIndexImageSnapshot {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::image_io::ImageReader<'a>> {
        Self::image_reader(self, control)
    }
}

impl PathIndex {
    /// Admit the complete immutable materialization in constant work. No graph is retained, consulted or evaluated, and no labels or pairs are copied.
    pub fn capture_native_memory_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PathIndexImageSnapshot> {
        control.check()?;
        let paths = self.memory_paths()?;
        let bytes = paths
            .retained_bytes()
            .checked_add(PathIndexImageSnapshot::wrapper_bytes())
            .ok_or(MemoryError::SizeOverflow)?;
        let memory = control.memory().reserve(bytes)?;
        control.check()?;
        Ok(PathIndexImageSnapshot::from_admitted(
            Arc::clone(paths),
            control,
            memory,
        ))
    }

    pub(super) fn memory_paths(&self) -> StorageBackendResult<&Arc<MemoryPaths>> {
        match &self.storage {
            PathStorage::Memory(paths) => Ok(paths),
            PathStorage::Persistent(_) | PathStorage::ReadView { .. } => {
                Err(StorageBackendError::Other(
                    "memory path-index capture requires materialized memory storage".into(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests;
