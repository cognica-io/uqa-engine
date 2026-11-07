//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Descriptor and dictionary positions borrow their original encoded resource owners.

use super::{
    invalid, AnalyzerResource, AnalyzerResourceKind, ImageRead, StorageBackendResult,
    StorageReadControl, MAGIC,
};
use uqa_core::memory::MemoryError;

pub(super) struct AnalyzerImageCursor<'a, 'c, R> {
    resources: R,
    remaining: usize,
    descriptor_pending: bool,
    payload: &'a [u8],
    header: [u8; 41],
    position: usize,
    length: usize,
    control: &'c StorageReadControl,
}

impl<'a, 'c, R: Iterator<Item = AnalyzerResource<'a>>> AnalyzerImageCursor<'a, 'c, R> {
    pub(super) fn new(
        descriptor: &'a str,
        count: usize,
        resources: R,
        control: &'c StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut header = [0; 41];
        header[..8].copy_from_slice(MAGIC);
        header[8..16].copy_from_slice(&encoded_count(descriptor.len())?);
        Ok(Self {
            resources,
            remaining: count,
            descriptor_pending: true,
            payload: descriptor.as_bytes(),
            header,
            position: 0,
            length: 16,
            control,
        })
    }

    fn next_header(&mut self) -> StorageBackendResult<bool> {
        self.control.check()?;
        self.position = 0;
        if self.descriptor_pending {
            self.descriptor_pending = false;
            self.header[..8].copy_from_slice(&encoded_count(self.remaining)?);
            self.length = 8;
        } else if let Some(resource) = self.resources.next() {
            self.remaining = self.remaining.checked_sub(1).ok_or_else(invalid)?;
            self.header[0] = match resource.kind() {
                AnalyzerResourceKind::Nori => 0,
                AnalyzerResourceKind::Kuromoji => 1,
            };
            self.header[1..33].copy_from_slice(&resource.sha256());
            self.header[33..].copy_from_slice(&encoded_count(resource.bytes().len())?);
            self.length = self.header.len();
            self.payload = resource.bytes();
        } else {
            self.length = 0;
            if self.remaining != 0 {
                return Err(invalid());
            }
            return Ok(false);
        }
        Ok(true)
    }
}

impl<'a, R: Iterator<Item = AnalyzerResource<'a>> + Send> ImageRead
    for AnalyzerImageCursor<'a, '_, R>
{
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.control.check()?;
        let mut written = 0;
        let limit = output.len().min(16 * 1024);
        while written < limit {
            self.control.check()?;
            let remaining = &mut output[written..limit];
            if self.position < self.length {
                let count = (self.length - self.position).min(remaining.len());
                remaining[..count]
                    .copy_from_slice(&self.header[self.position..self.position + count]);
                self.position += count;
                written += count;
            } else if !self.payload.is_empty() {
                let count = self.payload.len().min(remaining.len()).min(4096);
                remaining[..count].copy_from_slice(&self.payload[..count]);
                self.payload = &self.payload[count..];
                written += count;
            } else if !self.next_header()? {
                break;
            }
        }
        self.control.check()?;
        Ok(written)
    }
}

fn encoded_count(count: usize) -> StorageBackendResult<[u8; 8]> {
    Ok(u64::try_from(count)
        .map_err(|_| MemoryError::SizeOverflow)?
        .to_le_bytes())
}
