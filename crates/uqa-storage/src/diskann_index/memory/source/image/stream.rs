//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated tensors and immutable origin identities retain exact encoding positions.

use super::{Canonical, DiskANNChangeIdentity, Tensor};
use crate::{
    diskann_index::format::DiskANNVectorVersion, read_control::StorageReadControl,
    vector_index::image::coordinate_bytes, StorageBackendResult,
};
use uqa_core::{memory::BudgetedSharedMapIter, DocId};

enum Stage {
    Documents,
    Changes,
    Complete,
}

pub(in crate::diskann_index::memory) struct CanonicalImageCursor<'a> {
    documents: BudgetedSharedMapIter<'a, DocId, Tensor>,
    changes: BudgetedSharedMapIter<'a, DocId, DiskANNVectorVersion>,
    change_count: usize,
    vectors: std::slice::Iter<'a, Vec<f32>>,
    coordinates: std::slice::Iter<'a, f32>,
    pending: [u8; 4096],
    length: usize,
    position: usize,
    stage: Stage,
}

impl<'a> CanonicalImageCursor<'a> {
    pub(in crate::diskann_index::memory) fn new(source: &'a Canonical) -> Self {
        let mut pending = [0; 4096];
        let mut length = 1;
        if let Some(revision) = source.revision {
            pending[0] = 1;
            pending[1..41].copy_from_slice(&DiskANNChangeIdentity::new(0, revision).encode());
            length = 41;
        }
        for value in [
            source.counts.current_vectors(),
            source.counts.changed_vectors(),
            source.documents.len() as u64,
        ] {
            pending[length..length + 8].copy_from_slice(&value.to_le_bytes());
            length += 8;
        }
        Self {
            documents: source.documents.iter(),
            changes: source.changes.iter(),
            change_count: source.changes.len(),
            vectors: [].iter(),
            coordinates: [].iter(),
            pending,
            length,
            position: 0,
            stage: Stage::Documents,
        }
    }

    fn next_piece(&mut self) -> StorageBackendResult<bool> {
        self.position = 0;
        self.length = 0;
        if !self.coordinates.as_slice().is_empty() {
            self.length = coordinate_bytes(&mut self.coordinates, &mut self.pending)?;
        } else if let Some(vector) = self.vectors.next() {
            self.pending[..8].copy_from_slice(&(vector.len() as u64).to_le_bytes());
            self.length = 8;
            self.coordinates = vector.iter();
        } else {
            match self.stage {
                Stage::Documents => {
                    if let Some((&document, tensor)) = self.documents.next() {
                        self.pending[..8].copy_from_slice(&document.to_le_bytes());
                        self.pending[8..64].copy_from_slice(&tensor.origin.encode());
                        self.length = 64;
                        self.vectors = tensor.values.iter();
                    } else {
                        self.pending[..8]
                            .copy_from_slice(&(self.change_count as u64).to_le_bytes());
                        self.length = 8;
                        self.stage = Stage::Changes;
                    }
                }
                Stage::Changes => {
                    let Some((&document, &version)) = self.changes.next() else {
                        self.stage = Stage::Complete;
                        return Ok(false);
                    };
                    self.pending[..40]
                        .copy_from_slice(&DiskANNChangeIdentity::new(document, version).encode());
                    self.length = 40;
                }
                Stage::Complete => return Ok(false),
            }
        }
        Ok(true)
    }

    pub(in crate::diskann_index::memory) fn read(
        &mut self,
        output: &mut [u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<usize> {
        control.check()?;
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit {
            control.check()?;
            if self.position == self.length && !self.next_piece()? {
                break;
            }
            let count = (self.length - self.position).min(limit - written);
            output[written..written + count]
                .copy_from_slice(&self.pending[self.position..self.position + count]);
            self.position += count;
            written += count;
        }
        control.check()?;
        Ok(written)
    }
}
