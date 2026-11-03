//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Export-only handles cannot run queries that mutate evaluated physical state.

use crate::{read_control::StorageReadControl, StorageBackendResult, VectorIndex};
use std::sync::Arc;
use uqa_core::memory::MemoryReservation;

pub(crate) trait VectorImageSource: Send + Sync {
    fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64>;
}

struct IndexImage(Arc<dyn VectorIndex>);

impl VectorImageSource for IndexImage {
    fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.0.write_image(output, control)
    }
}

struct Retained<T> {
    source: T,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

impl<T: VectorImageSource> VectorImageSource for Retained<T> {
    fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        crate::image_io::with_retained_control(output, Some(&self.control), |output| {
            self.source.write_image(output, control)
        })
    }
}

/// A captured image source exposes only bounded output. Clones retain the same source and original capture control without making it queryable or writable.
#[derive(Clone)]
pub struct VectorImageSnapshot {
    source: Arc<dyn VectorImageSource>,
}

impl VectorImageSnapshot {
    pub fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.source.write_image(output, control)
    }

    pub(crate) fn from_index(
        source: Arc<dyn VectorIndex>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Self::new(IndexImage(source), control)
    }

    pub(crate) fn new<T: VectorImageSource + 'static>(
        source: T,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<Retained<T>>())?;
        Ok(Self {
            source: Arc::new(Retained {
                source,
                control: control.clone(),
                _memory: memory,
            }),
        })
    }
}
