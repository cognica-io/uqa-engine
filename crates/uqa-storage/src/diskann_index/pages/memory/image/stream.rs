//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sealed physical records remain borrowed through every output window.

use super::{BudgetedMapIter, BudgetedVec, DiskANNMemorySource, DiskANNRecordKey};
use crate::{read_control::StorageReadControl, StorageBackendResult};

enum Stage {
    Records,
    Pages,
    Complete,
}

pub(in crate::diskann_index) struct PhysicalImageCursor<'a> {
    records: BudgetedMapIter<'a, DiskANNRecordKey, BudgetedVec<u8>>,
    pages: BudgetedMapIter<'a, u64, BudgetedVec<u8>>,
    pending: [u8; 17],
    position: usize,
    length: usize,
    payload: &'a [u8],
    stage: Stage,
}

impl<'a> PhysicalImageCursor<'a> {
    pub(in crate::diskann_index) fn new(source: &'a DiskANNMemorySource) -> Self {
        let mut pending = [0; 17];
        pending[..8].copy_from_slice(&((source.records.records.len() - 1) as u64).to_le_bytes());
        Self {
            records: source.records.records.iter(),
            pages: source.records.graph.iter(),
            pending,
            position: 0,
            length: 8,
            payload: &[],
            stage: Stage::Records,
        }
    }

    fn next_piece(&mut self) -> bool {
        self.position = 0;
        self.length = 0;
        match self.stage {
            Stage::Records => {
                let mut next = self.records.next();
                // The sealed unique map contains exactly one manifest, encoded by the outer image.
                if next.is_some_and(|(key, _)| *key == DiskANNRecordKey::Manifest) {
                    next = self.records.next();
                }
                if let Some((key, bytes)) = next {
                    let (tag, first) = match key {
                        DiskANNRecordKey::Manifest => unreachable!("unique sealed manifest"),
                        DiskANNRecordKey::Codebook => (0, 0),
                        DiskANNRecordKey::Codes(first) => (1, *first),
                        DiskANNRecordKey::Side(first) => (2, *first),
                        DiskANNRecordKey::Origins(first) => (3, *first),
                    };
                    self.pending[0] = tag;
                    self.pending[1..9].copy_from_slice(&first.to_le_bytes());
                    self.pending[9..17].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
                    self.length = 17;
                    self.payload = bytes;
                } else {
                    self.pending[..8].copy_from_slice(&(self.pages.len() as u64).to_le_bytes());
                    self.length = 8;
                    self.stage = Stage::Pages;
                }
            }
            Stage::Pages => {
                let Some((&id, bytes)) = self.pages.next() else {
                    self.stage = Stage::Complete;
                    return false;
                };
                self.pending[..8].copy_from_slice(&id.to_le_bytes());
                self.length = 8;
                self.payload = bytes;
            }
            Stage::Complete => return false,
        }
        true
    }

    pub(in crate::diskann_index) fn read(
        &mut self,
        output: &mut [u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<usize> {
        control.check()?;
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit {
            control.check()?;
            let bytes = if self.position < self.length {
                let count = (self.length - self.position).min(limit - written);
                let bytes = &self.pending[self.position..self.position + count];
                self.position += count;
                bytes
            } else if !self.payload.is_empty() {
                let (bytes, remaining) = self
                    .payload
                    .split_at(self.payload.len().min(limit - written));
                self.payload = remaining;
                bytes
            } else if self.next_piece() {
                continue;
            } else {
                break;
            };
            output[written..written + bytes.len()].copy_from_slice(bytes);
            written += bytes.len();
        }
        control.check()?;
        Ok(written)
    }
}
