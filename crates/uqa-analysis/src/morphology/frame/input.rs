//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One private section retains its decompressor, source offset and digest until exact completion.

use super::{
    directory, vector, DecodedFrame, DictionaryError, DictionaryLimits, DictionaryResult,
    DirectoryEntry, Format, Section,
};
use miniz_oxide::{
    inflate::stream::{inflate, InflateState},
    DataFormat, MZFlush, MZStatus,
};
use sha2::{Digest, Sha256};
use std::ops::ControlFlow;

const FRAGMENT: usize = 4096;

#[cfg(test)]
mod tests;

pub(crate) struct FrameInput<'a> {
    bytes: &'a [u8],
    id: [u8; 32],
    entries: std::vec::IntoIter<DirectoryEntry>,
    sections: Vec<Section>,
    current: Option<SectionInput<'a>>,
}

impl<'a> FrameInput<'a> {
    pub(crate) fn new(
        format: &Format,
        bytes: &'a [u8],
        limits: DictionaryLimits,
    ) -> DictionaryResult<Self> {
        let (id, entries) = directory(format, bytes, limits)?;
        Ok(Self {
            bytes,
            id,
            entries: entries.into_iter(),
            sections: vector(format.section_count)?,
            current: None,
        })
    }

    /// Consume at most one fixed copy/inflate fragment. Failure consumes every unpublished section; only exact frame completion returns the decoded buffers.
    pub(crate) fn advance(mut self) -> DictionaryResult<ControlFlow<DecodedFrame, Self>> {
        let current = if let Some(current) = self.current.take() {
            current
        } else {
            let Some(entry) = self.entries.next() else {
                return Ok(ControlFlow::Break((self.id, self.sections)));
            };
            SectionInput::new(self.bytes, entry)?
        };
        match current.advance()? {
            ControlFlow::Continue(current) => self.current = Some(current),
            ControlFlow::Break(section) => self.sections.push(section),
        }
        if self.current.is_none() && self.entries.len() == 0 {
            Ok(ControlFlow::Break((self.id, self.sections)))
        } else {
            Ok(ControlFlow::Continue(self))
        }
    }
}

struct SectionInput<'a> {
    entry: DirectoryEntry,
    stored: &'a [u8],
    offset: usize,
    data: Vec<u8>,
    hash: Sha256,
    inflate: Option<Box<InflateState>>,
}

impl<'a> SectionInput<'a> {
    fn new(bytes: &'a [u8], entry: DirectoryEntry) -> DictionaryResult<Self> {
        let stored = &bytes[entry.offset..entry.offset + entry.stored];
        let data = vector(entry.decoded)?;
        let inflate = (entry.codec == 1).then(|| InflateState::new_boxed(DataFormat::Zlib));
        Ok(Self {
            entry,
            stored,
            offset: 0,
            data,
            hash: Sha256::new(),
            inflate,
        })
    }

    fn advance(mut self) -> DictionaryResult<ControlFlow<Section, Self>> {
        let input_end = self.offset.saturating_add(FRAGMENT).min(self.stored.len());
        let input = &self.stored[self.offset..input_end];
        let complete = if let Some(state) = &mut self.inflate {
            let mut output = [0; FRAGMENT];
            let maximum = (self.entry.decoded - self.data.len())
                .saturating_add(1)
                .min(FRAGMENT);
            let result = inflate(state, input, &mut output[..maximum], MZFlush::None);
            self.offset += result.bytes_consumed;
            if result.bytes_written > self.entry.decoded - self.data.len() {
                return Err(invalid_compressed());
            }
            self.data.extend_from_slice(&output[..result.bytes_written]);
            self.hash.update(&output[..result.bytes_written]);
            match result.status {
                Ok(MZStatus::StreamEnd) => {
                    if self.offset != self.stored.len() || self.data.len() != self.entry.decoded {
                        return Err(invalid_compressed());
                    }
                    true
                }
                Ok(MZStatus::Ok) if result.bytes_consumed != 0 || result.bytes_written != 0 => {
                    false
                }
                _ => return Err(invalid_compressed()),
            }
        } else {
            self.data.extend_from_slice(input);
            self.hash.update(input);
            self.offset = input_end;
            self.offset == self.stored.len()
        };
        if !complete {
            return Ok(ControlFlow::Continue(self));
        }
        let hash: [u8; 32] = self.hash.finalize().into();
        if hash != self.entry.hash {
            return Err(DictionaryError::Checksum(self.entry.kind));
        }
        Ok(ControlFlow::Break(Section {
            kind: self.entry.kind,
            records: self.entry.records,
            bytes: self.data,
        }))
    }
}

fn invalid_compressed() -> DictionaryError {
    super::invalid(
        "compressed section",
        "invalid or incorrectly sized zlib stream",
    )
}
