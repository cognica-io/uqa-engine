//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{value_error, DocumentMetadata, StorageBackendResult, StorageReadControl, MAGIC};
use crate::image_io::ImageRead;
use uqa_core::{value_image::ValueImageReader, Value};

pub(in crate::document_store) struct DocumentImageCursor<'a, 'control, I> {
    header: [u8; 13],
    length: usize,
    position: usize,
    values: ValueImageReader<'a, 'control, I>,
    control: &'control StorageReadControl,
}

impl<'a, 'control, I: ExactSizeIterator<Item = (&'a str, &'a Value)>>
    DocumentImageCursor<'a, 'control, I>
{
    pub(in crate::document_store) fn new(
        fields: I,
        metadata: DocumentMetadata,
        control: &'control StorageReadControl,
    ) -> Self {
        let mut header = [0; 13];
        header[..8].copy_from_slice(MAGIC);
        header[8] = u8::from(metadata.tuple_xmin().is_some());
        let length = if let Some(xmin) = metadata.tuple_xmin() {
            header[9..].copy_from_slice(&xmin.to_le_bytes());
            13
        } else {
            9
        };
        Self {
            header,
            length,
            position: 0,
            values: ValueImageReader::from_fields(fields, control.memory(), control.cancellation()),
            control,
        }
    }

    pub(in crate::document_store) fn read(
        &mut self,
        output: &mut [u8],
    ) -> StorageBackendResult<usize> {
        self.control.check()?;
        let limit = output.len().min(16 * 1024);
        let header = (self.length - self.position).min(limit);
        output[..header].copy_from_slice(&self.header[self.position..self.position + header]);
        self.position += header;
        let values = self
            .values
            .read(&mut output[header..limit])
            .map_err(value_error)?;
        Ok(header + values)
    }
}

impl<'a, I: ExactSizeIterator<Item = (&'a str, &'a Value)> + Send> ImageRead
    for DocumentImageCursor<'a, '_, I>
{
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.read(output)
    }
}
