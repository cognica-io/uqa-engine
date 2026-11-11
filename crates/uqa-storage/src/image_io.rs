//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded image I/O observes cancellation between partial and interrupted attempts.

use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use std::io::{Read, Write};
use uqa_core::memory::{Budgeted, MemoryError};

mod framed;
mod owned;
mod reader;
mod retained;
pub use framed::{framed_image_reader, read_framed_image_from, FramedImageInput};
pub use owned::ImageOwner;
pub use reader::{ImagePreparation, ImageRead, ImageReader};
pub use retained::with_retained_control;

/// Bounded binary output for an owning image codec. Failed output belongs to the enclosing unpublished artifact.
pub struct ImageOutput<'a> {
    pub writer: &'a mut dyn Write,
    pub control: &'a StorageReadControl,
    pub written: u64,
}

impl ImageOutput<'_> {
    /// Reuse Core's evaluated-value map codec. Its borrowed payload writes use at most 16 KiB windows and preserve all value carriers.
    pub fn fields<'a>(
        &mut self,
        fields: impl ExactSizeIterator<Item = (&'a str, &'a uqa_core::Value)>,
    ) -> StorageBackendResult<()> {
        let written = uqa_core::value_image::write_fields_image(
            fields,
            self.writer,
            self.control.memory(),
            self.control.cancellation(),
        )
        .map_err(value_error)?;
        self.written = self
            .written
            .checked_add(written)
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(())
    }

    pub fn bytes(&mut self, bytes: &[u8]) -> StorageBackendResult<()> {
        uqa_core::image_io::write_bytes(
            self.writer,
            self.control.cancellation(),
            &mut self.written,
            bytes,
        )
        .map_err(io_error)
    }

    pub fn count(&mut self, count: usize) -> StorageBackendResult<()> {
        self.bytes(
            &u64::try_from(count)
                .map_err(|_| MemoryError::SizeOverflow)?
                .to_le_bytes(),
        )
    }

    pub fn sized(&mut self, bytes: &[u8]) -> StorageBackendResult<()> {
        self.count(bytes.len())?;
        self.bytes(bytes)
    }
}

/// Controlled binary input; payload buffers retain their decoding allowance.
pub struct ImageInput<'a> {
    pub reader: &'a mut dyn Read,
    pub control: &'a StorageReadControl,
}

impl ImageInput<'_> {
    pub fn value(&mut self) -> StorageBackendResult<Budgeted<uqa_core::Value>> {
        uqa_core::value_image::read_value_image(
            self.reader,
            self.control.memory(),
            self.control.cancellation(),
        )
        .map_err(value_error)
    }

    fn input(&mut self) -> uqa_core::image_io::ImageInput<'_> {
        uqa_core::image_io::ImageInput {
            reader: self.reader,
            memory: self.control.memory(),
            cancellation: self.control.cancellation(),
        }
    }

    pub fn text(&mut self) -> StorageBackendResult<Budgeted<String>> {
        self.input().text().map_err(io_error)
    }
    pub fn exact(&mut self, bytes: &mut [u8]) -> StorageBackendResult<()> {
        self.input().exact(bytes).map_err(io_error)
    }
    pub fn array<const N: usize>(&mut self) -> StorageBackendResult<[u8; N]> {
        self.input().array().map_err(io_error)
    }
    pub fn count(&mut self) -> StorageBackendResult<usize> {
        self.input().count().map_err(io_error)
    }
    pub fn bytes(&mut self) -> StorageBackendResult<Budgeted<Vec<u8>>> {
        self.input().bytes().map_err(io_error)
    }
    /// Reject a declared payload beyond the owning format's ceiling before allocating it.
    pub fn bytes_with_limit(&mut self, maximum: usize) -> StorageBackendResult<Budgeted<Vec<u8>>> {
        self.input().bytes_with_limit(maximum).map_err(io_error)
    }
    pub fn end(&mut self) -> StorageBackendResult<()> {
        self.input().end().map_err(io_error)
    }
}

fn io_error(error: uqa_core::image_io::ImageIoError) -> StorageBackendError {
    error.into()
}

impl From<uqa_core::image_io::ImageIoError> for StorageBackendError {
    fn from(error: uqa_core::image_io::ImageIoError) -> Self {
        use uqa_core::image_io::ImageIoError;
        match error {
            ImageIoError::Invalid => {
                StorageBackendError::Other("invalid owner image stream".into())
            }
            ImageIoError::Memory(error) => error.into(),
            ImageIoError::Cancelled(error) => error.into(),
            ImageIoError::Io(error) => StorageBackendError::backend("owner image I/O", error),
        }
    }
}

fn value_error(error: uqa_core::value_image::ValueImageError) -> StorageBackendError {
    match error {
        uqa_core::value_image::ValueImageError::Memory(error) => error.into(),
        uqa_core::value_image::ValueImageError::Cancelled(error) => error.into(),
        other => StorageBackendError::backend("owner image value", other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_output_preserves_partial_progress_after_writer_unwind() {
        struct Writer(bool);
        impl Write for Writer {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                assert!(!self.0, "injected writer unwind");
                self.0 = true;
                Ok(2)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let control = StorageReadControl::with_limit(1024);
        let mut writer = Writer(false);
        let mut output = ImageOutput {
            writer: &mut writer,
            control: &control,
            written: 7,
        };
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            output.bytes(&[1, 2, 3, 4])
        }))
        .is_err());
        assert_eq!(output.written, 9);
    }
}
