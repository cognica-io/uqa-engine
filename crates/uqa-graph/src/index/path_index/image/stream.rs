//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable native ordering permits immediate output with fixed cursor state.

use super::{
    CapturedPath, CapturedPathIndex, ImageRead, StorageBackendResult, StorageReadControl, MAGIC,
};
use std::slice::Iter;
use uqa_core::memory::MemoryError;

enum Stage {
    Paths,
    Labels,
    Pairs,
}

pub(super) struct PathImageCursor<'a> {
    paths: Iter<'a, CapturedPath>,
    labels: Iter<'a, String>,
    pairs: Iter<'a, (u64, u64)>,
    stage: Stage,
    text: &'a [u8],
    pending: [u8; 16],
    position: usize,
    length: usize,
    control: &'a StorageReadControl,
}

impl<'a> PathImageCursor<'a> {
    pub(super) fn new(
        image: &'a CapturedPathIndex,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut pending = [0; 16];
        pending[..8].copy_from_slice(MAGIC);
        pending[8..].copy_from_slice(&count(image.paths.len())?);
        Ok(Self {
            paths: image.paths.iter(),
            labels: [].iter(),
            pairs: [].iter(),
            stage: Stage::Paths,
            text: &[],
            pending,
            position: 0,
            length: 16,
            control,
        })
    }

    fn next_header(&mut self) -> StorageBackendResult<bool> {
        self.position = 0;
        self.length = 8;
        loop {
            self.control.check()?;
            match self.stage {
                Stage::Paths => {
                    let Some(path) = self.paths.next() else {
                        self.length = 0;
                        return Ok(false);
                    };
                    self.labels = path.sequence.iter();
                    self.pairs = path.pairs.iter();
                    self.stage = Stage::Labels;
                    self.pending[..8].copy_from_slice(&count(self.labels.len())?);
                }
                Stage::Labels => {
                    if let Some(label) = self.labels.next() {
                        self.pending[..8].copy_from_slice(&count(label.len())?);
                        self.text = label.as_bytes();
                    } else {
                        self.stage = Stage::Pairs;
                        self.pending[..8].copy_from_slice(&count(self.pairs.len())?);
                    }
                }
                Stage::Pairs => {
                    let Some((source, target)) = self.pairs.next() else {
                        self.stage = Stage::Paths;
                        continue;
                    };
                    self.pending[..8].copy_from_slice(&source.to_le_bytes());
                    self.pending[8..].copy_from_slice(&target.to_le_bytes());
                    self.length = 16;
                }
            }
            return Ok(true);
        }
    }
}

impl ImageRead for PathImageCursor<'_> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.control.check()?;
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit {
            self.control.check()?;
            if self.position < self.length {
                let count = (self.length - self.position).min(limit - written);
                output[written..written + count]
                    .copy_from_slice(&self.pending[self.position..self.position + count]);
                self.position += count;
                written += count;
            } else if !self.text.is_empty() {
                let count = self.text.len().min(limit - written).min(4096);
                output[written..written + count].copy_from_slice(&self.text[..count]);
                self.text = &self.text[count..];
                written += count;
            } else if !self.next_header()? {
                break;
            }
        }
        self.control.check()?;
        Ok(written)
    }
}

fn count(value: usize) -> StorageBackendResult<[u8; 8]> {
    Ok(u64::try_from(value)
        .map_err(|_| MemoryError::SizeOverflow)?
        .to_le_bytes())
}
