//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Export-only handles cannot run queries that mutate evaluated physical state.

use crate::{
    image_io::{ImageOwner, ImageReader},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult, VectorIndex,
};
use std::sync::Arc;
use uqa_core::memory::MemoryReservation;

pub(crate) trait VectorImageSource: Send + Sync {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        control.check()?;
        Err(StorageBackendError::Other(
            "resumable vector image output is not supported by this backend".into(),
        ))
    }

    fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64>;
}

struct IndexImage(Arc<dyn VectorIndex>);

impl VectorImageSource for IndexImage {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        self.0.image_reader(control)
    }

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
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        self.control.check()?;
        self.source
            .image_reader(control)?
            .with_source_control(Some(&self.control), control)
    }

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
    /// Borrow this selected source's resumable codec, retaining its original cancellation. Capability follows the concrete codec; no full-image output buffer is created for unsupported owners.
    pub fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        self.source.image_reader(control)
    }

    /// Transfer this selected source into an independent movable reader. Codec opening is lazy; unsupported codecs fail on the first preparation or nonempty read and release the source.
    pub fn into_image_reader(
        self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'static>> {
        ImageReader::from_owner(self, control)
    }

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

impl ImageOwner for VectorImageSnapshot {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        Self::image_reader(self, control)
    }
}
