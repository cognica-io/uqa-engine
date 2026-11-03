//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated document images preserve fields and private tuple metadata separately.

use std::io::{Read, Write};

use super::{DocumentMetadata, StoredDocument};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::{
    memory::Budgeted,
    value_image::{read_value_image, write_fields_image, ValueImageError},
    Value,
};

const MAGIC: &[u8; 8] = b"UQADOC01";

pub(crate) fn with_retained_control<T>(
    output: &mut dyn Write,
    original: Option<&StorageReadControl>,
    write: impl FnOnce(&mut dyn Write) -> StorageBackendResult<T>,
) -> StorageBackendResult<T> {
    if let Some(original) = original {
        original.check()?;
    }
    let mut output = ImageOutput {
        output,
        original,
        error: None,
    };
    let result = write(&mut output);
    if let Some(error) = output.error {
        return Err(error);
    }
    let result = result?;
    if let Some(original) = original {
        original.check()?;
    }
    Ok(result)
}

struct ImageOutput<'a> {
    output: &'a mut dyn Write,
    original: Option<&'a StorageReadControl>,
    error: Option<StorageBackendError>,
}

impl Write for ImageOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(error) = self.original.and_then(|control| control.check().err()) {
            self.error = Some(error);
            return Err(std::io::Error::other("retained document image cancelled"));
        }
        self.output.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}

pub(super) fn write<'a>(
    fields: impl ExactSizeIterator<Item = (&'a str, &'a Value)>,
    metadata: DocumentMetadata,
    output: &mut dyn Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    control.check()?;
    let mut header = [0; 13];
    header[..8].copy_from_slice(MAGIC);
    header[8] = u8::from(metadata.tuple_xmin().is_some());
    let length = if let Some(xmin) = metadata.tuple_xmin() {
        header[9..].copy_from_slice(&xmin.to_le_bytes());
        13
    } else {
        9
    };
    output.write_all(&header[..length]).map_err(io_error)?;
    let written = write_fields_image(fields, output, control.memory(), control.cancellation())
        .map_err(value_error)?;
    control.check()?;
    written
        .checked_add(length as u64)
        .ok_or_else(|| uqa_core::memory::MemoryError::SizeOverflow.into())
}

/// Decode exactly one versioned tuple, leaving following input to its enclosing image. The result owns its complete decoded field allowance. Document identity belongs to the enclosing image address; no SQL, analyzer or callback is invoked here.
pub fn read_document_image(
    input: &mut dyn Read,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<StoredDocument>> {
    control.check()?;
    let mut header = [0; 9];
    input.read_exact(&mut header).map_err(io_error)?;
    if &header[..8] != MAGIC || header[8] > 1 {
        return Err(invalid("document image version or metadata flag"));
    }
    let metadata = if header[8] == 1 {
        control.check()?;
        let mut xmin = [0; 4];
        input.read_exact(&mut xmin).map_err(io_error)?;
        DocumentMetadata::with_tuple_xmin(u32::from_le_bytes(xmin))
    } else {
        DocumentMetadata::default()
    };
    let value =
        read_value_image(input, control.memory(), control.cancellation()).map_err(value_error)?;
    control.check()?;
    let (value, memory) = value.into_parts();
    let Value::Map(fields) = value else {
        return Err(invalid("document image fields are not a map"));
    };
    Ok(Budgeted::new(
        StoredDocument::with_metadata(fields, metadata),
        memory,
    ))
}

/// Decode a standalone tuple image, requiring exact input consumption.
pub fn decode_document_image(
    mut input: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<StoredDocument>> {
    let document = read_document_image(&mut input, control)?;
    if !input.is_empty() {
        return Err(invalid("trailing document image bytes"));
    }
    Ok(document)
}

fn invalid(reason: &'static str) -> StorageBackendError {
    StorageBackendError::Other(reason.into())
}

fn io_error(error: std::io::Error) -> StorageBackendError {
    StorageBackendError::backend("document image I/O", error)
}

fn value_error(error: ValueImageError) -> StorageBackendError {
    match error {
        ValueImageError::Memory(error) => StorageBackendError::Memory(error),
        ValueImageError::Cancelled(error) => StorageBackendError::Cancelled(error),
        error => StorageBackendError::backend("document value image", error),
    }
}

#[cfg(test)]
mod tests;
