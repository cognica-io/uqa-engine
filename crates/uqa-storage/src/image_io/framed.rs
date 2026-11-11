//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical bounded frames compose complete owner images without a sizing pass or a whole-image buffer.

use super::{ImagePreparation, ImageRead, ImageReader};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::io::{self, Read};

const CHUNK: usize = 4096;

mod input;
pub use input::read_framed_image_from;

/// Frame one complete owner image with little-endian u32 lengths, full 4 KiB chunks, an optional short final chunk and a zero terminator. Admission covers the fixed buffer; preparation remains with the original owner.
pub fn framed_image_reader<'a>(
    source: ImageReader<'a>,
    control: &'a StorageReadControl,
) -> StorageBackendResult<ImageReader<'a>> {
    ImageReader::new(
        FramedImage {
            source: Some(source),
            buffer: [0; CHUNK + 4],
            offset: 0,
            length: 0,
            terminal: false,
        },
        control,
    )
}

struct FramedImage<'a> {
    source: Option<ImageReader<'a>>,
    buffer: [u8; CHUNK + 4],
    offset: usize,
    length: usize,
    terminal: bool,
}

impl ImageRead for FramedImage<'_> {
    fn prepare(&mut self, maximum_positions: usize) -> StorageBackendResult<ImagePreparation> {
        match &mut self.source {
            Some(source) => source.prepare(maximum_positions),
            None => Ok(ImagePreparation {
                positions: 0,
                ready: true,
            }),
        }
    }

    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        let mut written = 0;
        while written < output.len() {
            if self.offset == self.length {
                if self.terminal {
                    break;
                }
                let mut payload = 0;
                while let Some(source) = &mut self.source {
                    if payload == CHUNK {
                        break;
                    }
                    let count = source.read(&mut self.buffer[4 + payload..])?;
                    if count == 0 {
                        self.source = None;
                        break;
                    }
                    payload += count;
                }
                self.buffer[..4].copy_from_slice(&(payload as u32).to_le_bytes());
                self.length = payload + 4;
                self.offset = 0;
                self.terminal = payload == 0;
            }
            let count = (self.length - self.offset).min(output.len() - written);
            output[written..written + count]
                .copy_from_slice(&self.buffer[self.offset..self.offset + count]);
            self.offset += count;
            written += count;
        }
        Ok(written)
    }
}

/// Present exactly one framed child as a complete Read stream. The enclosing controlled decoder owns cancellation checks; Interrupted attempts preserve framing progress. Other errors and unwinds make this reader terminal.
pub struct FramedImageInput<'a> {
    source: &'a mut dyn Read,
    state: Option<FrameState>,
}

#[derive(Default)]
struct FrameState {
    header: [u8; 4],
    header_used: usize,
    remaining: usize,
    short: bool,
    complete: bool,
}

impl<'a> FramedImageInput<'a> {
    pub fn new(source: &'a mut dyn Read) -> Self {
        Self {
            source,
            state: Some(FrameState::default()),
        }
    }
}

impl FrameState {
    fn accept_header_bytes(&mut self, count: usize) -> io::Result<()> {
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if count > 4 - self.header_used {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.header_used += count;
        Ok(())
    }

    fn finish_header(&mut self) -> io::Result<()> {
        let length = u32::from_le_bytes(self.header) as usize;
        self.header_used = 0;
        if length > CHUNK || (self.short && length != 0) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.complete = length == 0;
        self.short = length < CHUNK;
        self.remaining = length;
        Ok(())
    }

    fn accept_payload(&mut self, count: usize, limit: usize) -> io::Result<usize> {
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if count > limit {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.remaining -= count;
        Ok(count)
    }

    fn read(&mut self, source: &mut dyn Read, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.complete {
            return Ok(0);
        }
        if self.remaining == 0 {
            while self.header_used < 4 {
                let count = source.read(&mut self.header[self.header_used..])?;
                self.accept_header_bytes(count)?;
            }
            self.finish_header()?;
            if self.complete {
                return Ok(0);
            }
        }
        let limit = output.len().min(self.remaining);
        let count = source.read(&mut output[..limit])?;
        self.accept_payload(count, limit)
    }
}

impl Read for FramedImageInput<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut state = self
            .state
            .take()
            .ok_or_else(|| io::Error::other("failed framed image input"))?;
        let result = state.read(self.source, output);
        if match &result {
            Ok(_) => true,
            Err(error) => error.kind() == io::ErrorKind::Interrupted,
        } {
            self.state = Some(state);
        }
        result
    }
}

#[cfg(test)]
mod tests;
