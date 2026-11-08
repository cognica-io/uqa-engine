//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained catalog handles supply their owning codecs without reconstructing definitions.

use std::sync::Arc;
use uqa_core::memory::Budgeted;
use uqa_storage::{
    image_io::{ImageOwner, ImageRead, ImageReader},
    memory_capture::{CapturedMemoryValue, MemoryCapture, MemoryCaptureKey},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

type Open<T> = for<'a> fn(&'a T, &'a StorageReadControl) -> StorageBackendResult<ImageReader<'a>>;

struct CapturedImage<T> {
    value: Arc<Budgeted<CapturedMemoryValue<T>>>,
    open: Open<T>,
}

impl<T: Send + Sync + 'static> ImageOwner for CapturedImage<T> {
    fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        (self.open)(self.value.get()?, control)?
            .with_source_control(Some(self.value.read_control()), control)
    }
}

pub(super) fn captured<T: Send + Sync + 'static>(
    key: &MemoryCaptureKey<CapturedMemoryValue<T>>,
    cut: &MemoryCapture,
    control: &StorageReadControl,
    open: Open<T>,
) -> StorageBackendResult<ImageReader<'static>> {
    selected(key.captured(cut)?, control, open)
}

pub(super) fn selected<T: Send + Sync + 'static>(
    value: Arc<Budgeted<CapturedMemoryValue<T>>>,
    control: &StorageReadControl,
    open: Open<T>,
) -> StorageBackendResult<ImageReader<'static>> {
    ImageReader::from_owner(CapturedImage { value, open }, control)
}

pub(super) struct ReadFn<F>(pub(super) F);
impl<F: FnMut(&mut [u8]) -> StorageBackendResult<usize> + Send> ImageRead for ReadFn<F> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.0(output)
    }
}

pub(super) fn sql_error(error: uqa_sql::SQLError) -> StorageBackendError {
    match error {
        uqa_sql::SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("native memory image", error),
    }
}
