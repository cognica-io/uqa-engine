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
    value_image::{read_value_image, ValueImageError},
    Value,
};

const MAGIC: &[u8; 8] = b"UQADOC01";

mod stream;
pub(super) use stream::DocumentImageCursor;

pub(super) fn write<'a>(
    fields: impl ExactSizeIterator<Item = (&'a str, &'a Value)>,
    metadata: DocumentMetadata,
    output: &mut dyn Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let mut reader = DocumentImageCursor::new(fields, metadata, control);
    let mut buffer = [0; 16 * 1024];
    let mut writer = crate::image_io::ImageOutput {
        writer: output,
        control,
        written: 0,
    };
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(writer.written);
        }
        writer.bytes(&buffer[..count])?;
    }
}

/// Decode exactly one versioned tuple, leaving following input to its enclosing image. The result owns its complete decoded field allowance. Document identity belongs to the enclosing image address; no SQL, analyzer or callback is invoked here.
pub fn read_document_image(
    input: &mut dyn Read,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<StoredDocument>> {
    control.check()?;
    let mut header = [0; 9];
    read_exact(input, &mut header, control)?;
    if &header[..8] != MAGIC || header[8] > 1 {
        return Err(invalid("document image version or metadata flag"));
    }
    let metadata = if header[8] == 1 {
        control.check()?;
        let mut xmin = [0; 4];
        read_exact(input, &mut xmin, control)?;
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

fn read_exact(
    input: &mut dyn Read,
    mut bytes: &mut [u8],
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    while !bytes.is_empty() {
        control.check()?;
        match input.read(bytes) {
            Ok(0) => return Err(io_error(std::io::ErrorKind::UnexpectedEof.into())),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    control.check()
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
