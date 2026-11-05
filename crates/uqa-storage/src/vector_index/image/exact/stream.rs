//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native ordered documents and borrowed tensor coordinates retain their exact byte position.

use super::{
    invalid, ImageRead, MemoryError, MemoryVectorIndex, StorageBackendResult, StorageReadControl,
    VectorIndex,
};
use uqa_core::{memory::BudgetedMapIter, DocId};

pub(super) struct ExactImageCursor<'a> {
    documents: BudgetedMapIter<'a, DocId, Vec<Vec<f32>>>,
    vectors: std::slice::Iter<'a, Vec<f32>>,
    coordinates: std::slice::Iter<'a, f32>,
    dimensions: u32,
    pending: [u8; 4096],
    length: usize,
    position: usize,
    control: &'a StorageReadControl,
}

impl<'a> ExactImageCursor<'a> {
    pub(super) fn new(
        index: &'a MemoryVectorIndex,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut pending = [0; 4096];
        pending[..8].copy_from_slice(super::super::MAGIC);
        pending[8] = 0;
        pending[9..13].copy_from_slice(&index.dimensions.to_le_bytes());
        pending[13..21].copy_from_slice(&count(index.state.vectors.len())?);
        pending[21..29].copy_from_slice(&count(index.count()?)?);
        Ok(Self {
            documents: index.state.vectors.iter(),
            vectors: [].iter(),
            coordinates: [].iter(),
            dimensions: index.dimensions,
            pending,
            length: 29,
            position: 0,
            control,
        })
    }

    fn next_piece(&mut self) -> StorageBackendResult<bool> {
        self.control.check()?;
        self.position = 0;
        if !self.coordinates.as_slice().is_empty() {
            let count = self.coordinates.len().min(self.pending.len() / 4);
            for (bytes, value) in self.pending[..count * 4]
                .chunks_exact_mut(4)
                .zip(self.coordinates.by_ref())
            {
                if !value.is_finite() {
                    return Err(invalid());
                }
                bytes.copy_from_slice(&value.to_bits().to_le_bytes());
            }
            self.length = count * 4;
        } else if let Some(vector) = self.vectors.next() {
            if u64::try_from(vector.len()).map_err(|_| MemoryError::SizeOverflow)?
                != u64::from(self.dimensions)
            {
                return Err(invalid());
            }
            self.pending[..8].copy_from_slice(&count(vector.len())?);
            self.length = 8;
            self.coordinates = vector.iter();
        } else if let Some((&document, vectors)) = self.documents.next() {
            if vectors.is_empty() {
                return Err(invalid());
            }
            self.pending[..8].copy_from_slice(&document.to_le_bytes());
            self.pending[8..16].copy_from_slice(&count(vectors.len())?);
            self.length = 16;
            self.vectors = vectors.iter();
        } else {
            self.length = 0;
            return Ok(false);
        }
        Ok(true)
    }
}

impl ImageRead for ExactImageCursor<'_> {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.control.check()?;
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit {
            if self.position == self.length && !self.next_piece()? {
                break;
            }
            let count = (self.length - self.position).min(limit - written);
            output[written..written + count]
                .copy_from_slice(&self.pending[self.position..self.position + count]);
            self.position += count;
            written += count;
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
