//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Frozen IVF centroids and raw/normalized vectors share one bounded output continuation.

use super::{
    validation::Validation, IVFData, IVFState, ImageRead, StorageBackendResult, StorageReadControl,
    StoredVector, VectorKey,
};
use crate::{
    image_io::ImagePreparation,
    vector_index::image::{coordinate_bytes, MAGIC},
};
use uqa_core::memory::MemoryError;

enum Stage<'a> {
    Centroid,
    Vector,
    Raw(&'a StoredVector),
    Normalized(&'a StoredVector),
    Assignment(&'a StoredVector),
    Complete,
}

pub(super) struct IVFImageCursor<'a> {
    index: &'a IVFData,
    validation: Validation<'a>,
    centroids: std::slice::Iter<'a, Vec<f32>>,
    vectors: std::collections::btree_map::Iter<'a, VectorKey, StoredVector>,
    coordinates: std::slice::Iter<'a, f32>,
    stage: Stage<'a>,
    pending: [u8; 4096],
    length: usize,
    position: usize,
    control: &'a StorageReadControl,
}

impl<'a> IVFImageCursor<'a> {
    pub(super) fn new(
        index: &'a IVFData,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut pending = [0; 4096];
        pending[..8].copy_from_slice(MAGIC);
        pending[8] = 1;
        pending[9..13].copy_from_slice(&index.dimensions.to_le_bytes());
        for (slot, value) in pending[13..37].chunks_exact_mut(8).zip([
            index.nlist,
            index.nprobe,
            index.train_threshold,
        ]) {
            slot.copy_from_slice(&count(value)?);
        }
        pending[37] = match index.state {
            IVFState::Untrained => 0,
            IVFState::Trained => 1,
            IVFState::Stale => 2,
        };
        for (slot, value) in pending[38..62].chunks_exact_mut(8).zip([
            index.trained_size,
            index.deletes_since_train,
            index.centroids.len(),
        ]) {
            slot.copy_from_slice(&count(value)?);
        }
        Ok(Self {
            index,
            validation: Validation::new(index),
            centroids: index.centroids.iter(),
            vectors: index.vectors.iter(),
            coordinates: [].iter(),
            stage: Stage::Centroid,
            pending,
            length: 62,
            position: 0,
            control,
        })
    }

    fn values(&mut self, values: &'a [f32]) -> StorageBackendResult<()> {
        self.pending[..8].copy_from_slice(&count(values.len())?);
        self.length = 8;
        self.coordinates = values.iter();
        Ok(())
    }

    fn next_piece(&mut self) -> StorageBackendResult<bool> {
        self.control.check()?;
        self.position = 0;
        if !self.coordinates.as_slice().is_empty() {
            self.length = coordinate_bytes(&mut self.coordinates, &mut self.pending)?;
            return Ok(true);
        }
        self.stage = match std::mem::replace(&mut self.stage, Stage::Complete) {
            Stage::Centroid => {
                if let Some(centroid) = self.centroids.next() {
                    self.values(centroid)?;
                    Stage::Centroid
                } else {
                    self.pending[..8].copy_from_slice(&count(self.index.vectors.len())?);
                    self.length = 8;
                    Stage::Vector
                }
            }
            Stage::Vector => {
                let Some((&(document, ordinal), vector)) = self.vectors.next() else {
                    self.length = 0;
                    return Ok(false);
                };
                self.pending[..8].copy_from_slice(&document.to_le_bytes());
                self.pending[8..12].copy_from_slice(&ordinal.to_le_bytes());
                self.length = 12;
                Stage::Raw(vector)
            }
            Stage::Raw(vector) => {
                self.values(&vector.raw_vector)?;
                Stage::Normalized(vector)
            }
            Stage::Normalized(vector) => {
                self.values(&vector.vector)?;
                Stage::Assignment(vector)
            }
            Stage::Assignment(vector) => {
                self.pending[..4].copy_from_slice(&vector.norm.to_bits().to_le_bytes());
                self.pending[4] = u8::from(vector.centroid.is_some());
                self.length = if let Some(centroid) = vector.centroid {
                    self.pending[5..13].copy_from_slice(&count(centroid)?);
                    13
                } else {
                    5
                };
                Stage::Vector
            }
            Stage::Complete => {
                self.length = 0;
                return Ok(false);
            }
        };
        Ok(true)
    }
}

impl ImageRead for IVFImageCursor<'_> {
    fn prepare(&mut self, maximum: usize) -> StorageBackendResult<ImagePreparation> {
        self.validation.advance(maximum, self.control)
    }

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
            written += count;
            self.position += count;
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
