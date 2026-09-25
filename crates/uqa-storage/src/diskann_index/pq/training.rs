//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::BudgetedVec;

use super::super::random::SplitMix64;
use super::{checkpoint, chunk_range, invalid, lloyd, product, PQCodebook};
use crate::diskann_index::NavigationVector;
use crate::{read_control::StorageReadControl, StorageBackendResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PQTrainingOptions {
    pub max_samples: u32,
    pub max_iterations: u32,
    pub max_centroids: u16,
    pub seed: u64,
}

impl Default for PQTrainingOptions {
    fn default() -> Self {
        Self {
            max_samples: 65_536,
            max_iterations: 20,
            max_centroids: 256,
            seed: 42,
        }
    }
}

impl PQTrainingOptions {
    pub(in crate::diskann_index) fn validate(self) -> StorageBackendResult<Self> {
        if self.max_samples == 0
            || !(1..=256).contains(&self.max_iterations)
            || !(1..=256).contains(&self.max_centroids)
        {
            return Err(invalid(
                "require positive samples, 1..=256 iterations and 1..=256 centroids",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PQTrainingSummary {
    pub options: PQTrainingOptions,
    pub observed_vectors: u64,
    pub sampled_vectors: u32,
}

impl PQTrainingSummary {
    pub(in crate::diskann_index) fn validate(
        self,
        dimensions: u32,
        pq_bytes: usize,
        count: u16,
    ) -> StorageBackendResult<usize> {
        self.options.validate()?;
        let dimensions = usize::try_from(dimensions).map_err(|_| invalid("dimension range"))?;
        if dimensions == 0
            || pq_bytes == 0
            || pq_bytes > dimensions
            || self.observed_vectors == 0
            || u64::from(self.sampled_vectors)
                != self
                    .observed_vectors
                    .min(u64::from(self.options.max_samples))
            || u32::from(count) != u32::from(self.options.max_centroids).min(self.sampled_vectors)
        {
            return Err(invalid(
                "inconsistent codebook dimensions or training provenance",
            ));
        }
        product(dimensions, usize::from(count))
    }
}

/// Bounded reservoir over navigable vectors supplied in stable logical-key order.
#[derive(Debug)]
pub struct PQTrainer {
    dimensions: usize,
    pq_bytes: usize,
    options: PQTrainingOptions,
    pub(super) samples: BudgetedVec<BudgetedVec<f64>>,
    seen: u64,
    random: SplitMix64,
    control: StorageReadControl,
}

impl PQTrainer {
    pub fn new(
        dimensions: u32,
        pq_bytes: usize,
        options: PQTrainingOptions,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let dimensions = usize::try_from(dimensions)
            .map_err(|_| invalid("dimensions exceed the platform range"))?;
        if dimensions == 0 || pq_bytes == 0 || pq_bytes > dimensions {
            return Err(invalid(
                "chunks must be nonempty and cover positive dimensions",
            ));
        }
        options.validate()?;
        Ok(Self {
            dimensions,
            pq_bytes,
            options,
            samples: BudgetedVec::new(control.memory()),
            seen: 0,
            random: SplitMix64(options.seed),
            control: control.clone(),
        })
    }

    pub fn observed_vectors(&self) -> u64 {
        self.seen
    }

    pub fn sampled_vectors(&self) -> usize {
        self.samples.len()
    }

    /// A failed admission preserves the accepted sample, observation count and random stream.
    pub fn observe(&mut self, vector: &NavigationVector) -> StorageBackendResult<()> {
        self.control.check()?;
        if vector.coordinates().len() != self.dimensions {
            return Err(invalid("training vector dimensions differ"));
        }
        let seen = self
            .seen
            .checked_add(1)
            .ok_or_else(|| invalid("training observation count overflow"))?;
        let mut random = self.random;
        let limit = u64::from(self.options.max_samples);
        let slot = if self.seen < limit {
            self.seen
        } else {
            random.below(seen, &self.control)?
        };
        if slot < limit {
            let mut copy = BudgetedVec::new(self.control.memory());
            copy.reserve(self.dimensions)?;
            for (offset, &value) in vector.coordinates().iter().enumerate() {
                checkpoint(offset, &self.control)?;
                copy.push(value)?;
            }
            self.control.check()?;
            if self.seen < limit {
                self.samples.push(copy)?;
            } else {
                self.samples[slot as usize] = copy;
            }
        }
        self.seen = seen;
        self.random = random;
        Ok(())
    }

    pub fn finish(self) -> StorageBackendResult<PQCodebook> {
        self.control.check()?;
        if self.samples.is_empty() {
            return Err(invalid("cannot train without navigable vectors"));
        }
        let count = usize::from(self.options.max_centroids).min(self.samples.len());
        let mut centroids = BudgetedVec::new(self.control.memory());
        centroids.reserve(product(count, self.dimensions)?)?;
        let mut random = SplitMix64(self.options.seed ^ 0xd1b5_4a32_d192_ed03);
        let mut order = BudgetedVec::new(self.control.memory());
        order.reserve(self.samples.len())?;
        for chunk in 0..self.pq_bytes {
            self.control.check()?;
            order.clear();
            for index in 0..self.samples.len() {
                checkpoint(index, &self.control)?;
                order.push(index)?;
            }
            let range = chunk_range(self.dimensions, self.pq_bytes, chunk);
            for label in 0..count {
                let slot =
                    label + random.below((order.len() - label) as u64, &self.control)? as usize;
                order.swap(label, slot);
                for (offset, &value) in self.samples[order[label]][range.clone()].iter().enumerate()
                {
                    checkpoint(offset, &self.control)?;
                    centroids.push(value)?;
                }
            }
            lloyd::train(
                &self.samples,
                range.clone(),
                &mut centroids[range.start * count..range.end * count],
                self.options.max_iterations,
                &self.control,
            )?;
        }
        self.control.check()?;
        Ok(PQCodebook {
            dimensions: self.dimensions,
            pq_bytes: self.pq_bytes,
            centroid_count: count as u16,
            centroids,
            training: PQTrainingSummary {
                options: self.options,
                observed_vectors: self.seen,
                sampled_vectors: self.samples.len() as u32,
            },
        })
    }
}
