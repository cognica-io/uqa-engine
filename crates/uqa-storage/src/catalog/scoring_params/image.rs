//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original scoring names and encoded values move through admitted native owners without interpretation.

use super::{CapturedScoringParameters, ScoringParameters};
use crate::{
    image_io::{ImageInput, ImageRead, ImageReader},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};
use std::{cmp::Ordering, io::Read, ops::Bound};
use uqa_core::memory::Budgeted;

const MAGIC: [u8; 8] = *b"UQASCP01";

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid scoring parameter image".into())
}

fn compare(
    left: &str,
    right: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<Ordering> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        control.check()?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    control.check()?;
    Ok(left.len().cmp(&right.len()))
}

impl ScoringParameters {
    /// Build a private native catalog and transfer each decoded key/value lease into its entry. Clones and retained roots keep the allowance until the final owner retires.
    pub fn read_image(
        reader: &mut dyn Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let mut input = ImageInput { reader, control };
        if input.array::<8>()? != MAGIC {
            return Err(invalid());
        }
        let count = input.count()?;
        let mut result = Self::new(control.memory());
        for _ in 0..count {
            control.check()?;
            let name = input.text()?;
            if let Some((previous, _)) = result.values.last_before::<str>(Bound::Unbounded) {
                if compare(previous, &name, control)? != Ordering::Less {
                    return Err(invalid());
                }
            }
            let encoded = input.text()?;
            let (name, mut memory) = name.into_parts();
            let (encoded, encoded_memory) = encoded.into_parts();
            memory.absorb(encoded_memory);
            let encoded = Budgeted::new(encoded, memory);
            let bytes = encoded.reserved_bytes();
            result.values.insert(name, encoded, bytes)?;
        }
        input.end()?;
        Ok(result)
    }
}

impl CapturedScoringParameters {
    /// Initialize fixed cursor state without scanning or copying the selected values.
    pub fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<ImageReader<'a>> {
        control.check()?;
        let mut header = [0; 16];
        header[..8].copy_from_slice(&MAGIC);
        header[8..].copy_from_slice(&(self.len() as u64).to_le_bytes());
        ImageReader::new(
            ParametersImage {
                entries: self.iter(),
                header,
                header_length: 16,
                offset: 0,
                text: &[],
                pending: None,
                control,
            },
            control,
        )
    }
}

struct ParametersImage<'a, I> {
    entries: I,
    header: [u8; 16],
    header_length: usize,
    offset: usize,
    text: &'a [u8],
    pending: Option<&'a str>,
    control: &'a StorageReadControl,
}

impl<'a, I: Iterator<Item = (&'a String, &'a String)> + Send> ImageRead for ParametersImage<'a, I> {
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
                let value = if let Some(value) = self.pending.take() {
                    value
                } else {
                    let Some((name, value)) = self.entries.next() else {
                        break;
                    };
                    self.pending = Some(value);
                    name
                };
                self.header[..8].copy_from_slice(&(value.len() as u64).to_le_bytes());
                self.header_length = 8;
                self.offset = 0;
                self.text = value.as_bytes();
                continue;
            }
            let count = bytes.len().min(output.len() - written).min(4096);
            output[written..written + count].copy_from_slice(&bytes[..count]);
            self.offset += count;
            written += count;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests;
