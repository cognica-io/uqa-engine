//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native durable descriptions preserve original object addresses and text without catalog lookup.

use super::{CapturedComments, CommentAddress, CommentRegistry};
use std::io::Read;
use uqa_core::{image_io::ImageInput, memory::MemoryBudget, CancellationToken};
use uqa_sql::SQLError;
use uqa_storage::{
    image_io::{ImageRead, ImageReader},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

const MAGIC: [u8; 8] = *b"UQACMT01";

fn invalid(message: &'static str) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX001".into(),
        message: format!("invalid comment image: {message}"),
    }
}

fn valid_address(address: CommentAddress) -> bool {
    address.object_id != 0 && address.class_id != 0 && address.sub_id >= 0
}

impl CommentRegistry {
    /// Reconstruct an unpublished durable registry under the supplied allowance. Cross-catalog object existence belongs to complete restore validation.
    pub fn read_image(
        reader: &mut dyn Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        Self::read_image_with_mutation_budget(reader, memory, memory, cancellation)
    }

    /// Decode original entries under `memory` and fund later native edits from `mutations`. Original payloads and retained predecessors keep their input reservations independently of future mutations and input cancellation.
    pub fn read_image_with_mutation_budget(
        reader: &mut dyn Read,
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        let mut input = ImageInput {
            reader,
            memory,
            cancellation,
        };
        if input.array::<8>()? != MAGIC {
            return Err(invalid("version header"));
        }
        let count = input.count()?;
        let mut registry = Self::with_budget(memory);
        let mut previous = None;
        for _ in 0..count {
            cancellation.check()?;
            let address = CommentAddress {
                object_id: u32::from_le_bytes(input.array()?),
                sub_id: i32::from_le_bytes(input.array()?),
                class_id: u32::from_le_bytes(input.array()?),
            };
            if !valid_address(address) || previous.is_some_and(|key| key >= address) {
                return Err(invalid("object address or record order"));
            }
            let text = input.text()?;
            if text.is_empty() {
                return Err(invalid("empty description"));
            }
            for chunk in text.as_bytes().chunks(4096) {
                cancellation.check()?;
                if chunk.contains(&0) {
                    return Err(invalid("description contains NUL"));
                }
            }
            let bytes = text.reserved_bytes();
            registry.durable.insert(address, text, bytes)?;
            previous = Some(address);
        }
        input.end()?;
        registry.durable = registry.durable.into_mutation_budget(mutations);
        registry.temporary = registry.temporary.into_mutation_budget(mutations);
        Ok(registry)
    }
}

impl CapturedComments {
    /// Initialize fixed cursor state over the selected durable root. Output borrows text, admits only the cursor and does not scan descriptions during construction.
    pub fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        control.check()?;
        let mut header = [0; 20];
        header[..8].copy_from_slice(&MAGIC);
        header[8..16].copy_from_slice(&(self.len() as u64).to_le_bytes());
        ImageReader::new(
            CommentImage {
                entries: self.iter(),
                header,
                header_length: 16,
                offset: 0,
                text: &[],
                control,
            },
            control,
        )
    }
}

struct CommentImage<'a, I> {
    entries: I,
    header: [u8; 20],
    header_length: usize,
    offset: usize,
    text: &'a [u8],
    control: &'a StorageReadControl,
}

impl<'a, I: Iterator<Item = (&'a CommentAddress, &'a String)> + Send> ImageRead
    for CommentImage<'a, I>
{
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        let mut written = 0;
        while written < output.len() {
            self.control.check()?;
            let bytes = if self.offset < self.header_length {
                &self.header[self.offset..self.header_length]
            } else {
                &self.text[self.offset - self.header_length..]
            };
            if bytes.is_empty() {
                let Some((address, text)) = self.entries.next() else {
                    break;
                };
                if !valid_address(*address) || text.is_empty() {
                    return Err(StorageBackendError::Other(
                        "invalid captured comment address or text".into(),
                    ));
                }
                self.header[..4].copy_from_slice(&address.object_id.to_le_bytes());
                self.header[4..8].copy_from_slice(&address.sub_id.to_le_bytes());
                self.header[8..12].copy_from_slice(&address.class_id.to_le_bytes());
                self.header[12..20].copy_from_slice(&(text.len() as u64).to_le_bytes());
                self.header_length = self.header.len();
                self.offset = 0;
                self.text = text.as_bytes();
                continue;
            }
            let count = bytes.len().min(output.len() - written).min(4096);
            if self.offset >= self.header_length && bytes[..count].contains(&0) {
                return Err(StorageBackendError::Other(
                    "invalid captured comment text".into(),
                ));
            }
            output[written..written + count].copy_from_slice(&bytes[..count]);
            self.offset += count;
            written += count;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests;
