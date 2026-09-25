//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::ops::Range;

use uqa_core::memory::{BudgetedVec, MemoryError};

use super::metric::{checkpoint, squared_distance};
use super::NavigationVector;
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

mod lloyd;
mod training;

pub use training::{PQTrainer, PQTrainingOptions, PQTrainingSummary};

/// Euclidean chunk centroids. Resident ownership retains its allocation allowance, independently of query cancellation.
#[derive(Debug)]
pub struct PQCodebook {
    dimensions: usize,
    pq_bytes: usize,
    centroid_count: u16,
    centroids: BudgetedVec<f64>,
    training: PQTrainingSummary,
}

#[derive(Debug)]
pub struct PQLookupTable {
    pq_bytes: usize,
    centroid_count: u16,
    distances: BudgetedVec<f64>,
}

/// Approximate navigation value; it is neither a canonical cosine score nor calibrated evidence.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct PQDistance(f64);

impl PQDistance {
    pub fn get(self) -> f64 {
        self.0
    }
}

impl PQCodebook {
    pub const CODEC_REVISION: u32 = 1;
    pub const TRAINING_REVISION: u32 = 1;

    pub(in crate::diskann_index) fn restore(
        dimensions: u32,
        pq_bytes: usize,
        centroid_count: u16,
        training: PQTrainingSummary,
        centroids: BudgetedVec<f64>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        if centroids.len() != training.validate(dimensions, pq_bytes, centroid_count)? {
            return Err(invalid("centroid scalar count differs from dimensions"));
        }
        for (offset, &value) in centroids.iter().enumerate() {
            checkpoint(offset, control)?;
            // Unit-coordinate means stay within this conservative rounding envelope for u32 dimensions and sample counts.
            if !value.is_finite() || value.abs() > 2.0 {
                return Err(invalid(
                    "centroid exceeds finite navigation coordinate bounds",
                ));
            }
        }
        control.check()?;
        Ok(Self {
            dimensions: dimensions as usize,
            pq_bytes,
            centroid_count,
            centroids,
            training,
        })
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub fn pq_bytes(&self) -> usize {
        self.pq_bytes
    }

    pub fn centroid_count(&self) -> u16 {
        self.centroid_count
    }

    pub fn training(&self) -> PQTrainingSummary {
        self.training
    }

    pub fn chunk_range(&self, chunk: usize) -> Option<Range<usize>> {
        (chunk < self.pq_bytes).then(|| chunk_range(self.dimensions, self.pq_bytes, chunk))
    }

    /// Centroid-major coordinates for one contiguous chunk, using scalar `f64` values.
    pub fn chunk_centroids(&self, chunk: usize) -> Option<&[f64]> {
        let range = self.chunk_range(chunk)?;
        let count = usize::from(self.centroid_count);
        Some(&self.centroids[range.start * count..range.end * count])
    }

    pub fn encode(
        &self,
        vector: &NavigationVector,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        self.encode_coordinates(vector.coordinates(), control)
    }

    fn encode_coordinates(
        &self,
        coordinates: &[f64],
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        self.check_dimensions(coordinates, control)?;
        let mut codes = BudgetedVec::new(control.memory());
        codes.reserve(self.pq_bytes)?;
        for chunk in 0..self.pq_bytes {
            let range = chunk_range(self.dimensions, self.pq_bytes, chunk);
            let count = usize::from(self.centroid_count);
            let centroids = &self.centroids[range.start * count..range.end * count];
            codes.push(nearest(&coordinates[range], centroids, control)?)?;
        }
        control.check()?;
        Ok(codes)
    }

    pub fn lookup(
        &self,
        query: &NavigationVector,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PQLookupTable> {
        self.lookup_coordinates(query.coordinates(), control)
    }

    fn lookup_coordinates(
        &self,
        coordinates: &[f64],
        control: &StorageReadControl,
    ) -> StorageBackendResult<PQLookupTable> {
        self.check_dimensions(coordinates, control)?;
        let mut distances = BudgetedVec::new(control.memory());
        let count = usize::from(self.centroid_count);
        distances.reserve(product(self.pq_bytes, count)?)?;
        for chunk in 0..self.pq_bytes {
            let range = chunk_range(self.dimensions, self.pq_bytes, chunk);
            let centroids = &self.centroids[range.start * count..range.end * count];
            for centroid in centroids.chunks_exact(range.len()) {
                distances.push(squared_distance(
                    &coordinates[range.clone()],
                    centroid,
                    control,
                )?)?;
            }
        }
        control.check()?;
        Ok(PQLookupTable {
            pq_bytes: self.pq_bytes,
            centroid_count: self.centroid_count,
            distances,
        })
    }

    fn check_dimensions(
        &self,
        coordinates: &[f64],
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if coordinates.len() != self.dimensions {
            return Err(invalid("vector dimensions do not match the codebook"));
        }
        Ok(())
    }
}

impl PQLookupTable {
    /// Labels must belong to this query's codebook; the generation owner supplies that identity boundary.
    pub fn estimate(
        &self,
        codes: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<PQDistance> {
        control.check()?;
        if codes.len() != self.pq_bytes {
            return Err(invalid("code length does not match the codebook"));
        }
        let mut distance = 0.0;
        for (chunk, &label) in codes.iter().enumerate() {
            checkpoint(chunk, control)?;
            if u16::from(label) >= self.centroid_count {
                return Err(invalid("code label exceeds the actual centroid count"));
            }
            distance +=
                self.distances[chunk * usize::from(self.centroid_count) + usize::from(label)];
        }
        Ok(PQDistance(distance))
    }
}

pub(in crate::diskann_index) fn chunk_range(
    dimensions: usize,
    chunks: usize,
    chunk: usize,
) -> Range<usize> {
    let width = dimensions / chunks;
    let extra = dimensions % chunks;
    let start = chunk * width + chunk.min(extra);
    start..start + width + usize::from(chunk < extra)
}

fn nearest(
    coordinates: &[f64],
    centroids: &[f64],
    control: &StorageReadControl,
) -> StorageBackendResult<u8> {
    let mut label = 0;
    let mut minimum = f64::INFINITY;
    for (index, centroid) in centroids.chunks_exact(coordinates.len()).enumerate() {
        let distance = squared_distance(coordinates, centroid, control)?;
        if distance < minimum {
            minimum = distance;
            label = index as u8;
        }
    }
    Ok(label)
}

fn product(left: usize, right: usize) -> StorageBackendResult<usize> {
    left.checked_mul(right)
        .ok_or_else(|| MemoryError::SizeOverflow.into())
}

fn filled<T: Copy>(
    len: usize,
    value: T,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<T>> {
    let mut values = BudgetedVec::new(control.memory());
    values.reserve(len)?;
    for offset in 0..len {
        checkpoint(offset, control)?;
        values.push(value)?;
    }
    Ok(values)
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid DiskANN PQ: {message}"))
}

#[cfg(test)]
mod tests;
