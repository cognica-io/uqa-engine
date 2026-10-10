//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streamed IVF preparation keeps canonical payloads in bounded, spilling ordered roots.

mod canonical;
mod mutation;
mod read;
mod record;
#[cfg(test)]
mod tests;
mod training;

pub(crate) use canonical::IVFCanonicalBuilder;
pub(crate) use read::IVFReadIndex;

use super::{math::l2_normalize, state::StoredVector, IVFMetadataSnapshot, IVFMutation, IVFState};
use crate::{
    read_control::StorageReadControl, spill_map::Map, IVFIndexParams, StorageBackendError,
    StorageBackendResult,
};
use uqa_core::{memory::MemoryReservation, DocId};

/// Provider-independent reconstruction validates one ordered canonical vector at a time. Temporary roots spill under the original caller's allowance.
pub struct IVFRestoreBuilder {
    candidate: IVFPreparedMetadata,
    assignments: Map<u64>,
    previous: Option<(DocId, u32)>,
}

/// Immutable publication input. Assignments are streamed from an owned, spilling generation rather than materialized as a corpus-sized vector.
pub struct IVFPreparedMetadata {
    pub(super) snapshot: IVFMetadataSnapshot,
    pub(super) vectors: Map<StoredVector>,
    pub(super) params: IVFIndexParams,
    pub(super) dimensions: u32,
    pub(super) control: StorageReadControl,
    pub(super) memory: MemoryReservation,
}

impl IVFRestoreBuilder {
    pub fn new(
        dimensions: u32,
        params: IVFIndexParams,
        mut header: IVFMetadataSnapshot,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        params.validate()?;
        if !header.centroids.is_empty() || !header.assignments.is_empty() {
            return Err(corrupt("streamed header contains unowned metadata"));
        }
        let capacity = params.nlist;
        let memory = control.memory().reserve(
            capacity
                .checked_mul(size_of::<Vec<f32>>())
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
        )?;
        header.centroids = Vec::with_capacity(capacity);
        header.assignments = Vec::new();
        Ok(Self {
            candidate: IVFPreparedMetadata {
                snapshot: header,
                // Preparation can keep the source and newly assigned root simultaneously; leave a third for provider pages, centroids and individual record scratch.
                vectors: Map::new(control.memory(), control.memory().limit() / 3),
                params,
                dimensions,
                control: control.clone(),
                memory,
            },
            assignments: Map::new(control.memory(), control.memory().limit() / 16),
            previous: None,
        })
    }

    pub fn centroid(&mut self, id: usize, vector: &[f32]) -> StorageBackendResult<()> {
        self.candidate.control.check()?;
        if id != self.candidate.snapshot.centroids.len()
            || id >= self.candidate.params.nlist
            || id >= self.candidate.snapshot.centroids.capacity()
        {
            return Err(corrupt("invalid centroid sequence"));
        }
        crate::vector_index::validate_vector_values(self.candidate.dimensions, vector)
            .map_err(|error| corrupt_vector("centroid", &error))?;
        self.candidate.memory.grow(
            vector
                .len()
                .checked_mul(size_of::<f32>())
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
        )?;
        self.candidate.snapshot.centroids.push(vector.to_vec());
        Ok(())
    }

    pub fn assignment(
        &mut self,
        document: DocId,
        ordinal: u32,
        centroid: usize,
    ) -> StorageBackendResult<()> {
        self.candidate.control.check()?;
        let key = key(document, ordinal);
        if centroid >= self.candidate.snapshot.centroids.len()
            || self.assignments.get(key)?.is_some()
        {
            return Err(corrupt("invalid or duplicate vector assignment"));
        }
        self.assignments
            .insert(key, centroid as u64, Some(&self.candidate.control))
    }

    pub fn vector(
        &mut self,
        document: DocId,
        ordinal: u32,
        raw: &[f32],
    ) -> StorageBackendResult<()> {
        self.candidate.control.check()?;
        let expected = match self.previous {
            Some((prior, order)) if prior == document => u64::from(order) + 1,
            Some((prior, _)) if prior > document => {
                return Err(corrupt("unordered canonical vectors"))
            }
            _ => 0,
        };
        if u64::from(ordinal) != expected {
            return Err(corrupt("canonical tensor has non-contiguous ordinals"));
        }
        let centroid = self
            .assignments
            .get(key(document, ordinal))?
            .map(|value| usize::try_from(*value))
            .transpose()
            .map_err(|_| uqa_core::memory::MemoryError::SizeOverflow)?;
        if centroid.is_none() && self.candidate.snapshot.state != IVFState::Untrained {
            return Err(corrupt("trained vector has no assignment"));
        }
        self.candidate.insert(document, ordinal, raw, centroid)?;
        self.previous = Some((document, ordinal));
        Ok(())
    }

    pub fn finish(self) -> StorageBackendResult<IVFPreparedMetadata> {
        self.candidate.control.check()?;
        let snapshot = &self.candidate.snapshot;
        if snapshot.vector_count != self.candidate.vectors.len()
            || (snapshot.state == IVFState::Untrained
                && (!snapshot.centroids.is_empty()
                    || !self.assignments.is_empty()
                    || snapshot.trained_size != 0
                    || snapshot.deletes_since_train != 0))
            || (snapshot.state != IVFState::Untrained
                && (snapshot.centroids.is_empty()
                    || self.assignments.len() != snapshot.vector_count))
        {
            return Err(corrupt("metadata does not cover the canonical generation"));
        }
        Ok(self.candidate)
    }
}

impl IVFPreparedMetadata {
    /// Scalar state and centroids; corpus assignments are available through `assignments()` instead of the empty header field.
    pub fn header(&self) -> &IVFMetadataSnapshot {
        &self.snapshot
    }
    pub fn params(&self) -> IVFIndexParams {
        self.params
    }

    pub fn prepare(mut self, mutations: &[IVFMutation<'_>]) -> StorageBackendResult<Self> {
        for mutation in mutations {
            self.apply_evaluated(*mutation)?;
        }
        Ok(self)
    }

    pub(crate) fn apply_evaluated(
        &mut self,
        mutation: IVFMutation<'_>,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        self.apply(mutation)?;
        if self.snapshot.state == IVFState::Stale {
            self.train()?;
        }
        Ok(())
    }

    pub fn assignments(
        &self,
    ) -> impl Iterator<Item = StorageBackendResult<(DocId, u32, usize)>> + '_ {
        self.vectors.iter().filter_map(|entry| {
            if let Err(error) = self.control.check() {
                return Some(Err(error));
            }
            match entry {
                Err(error) => Some(Err(error)),
                Ok((_, vector)) => vector
                    .centroid
                    .map(|centroid| Ok((vector.doc_id, vector.vector_ordinal, centroid))),
            }
        })
    }

    /// Stream only the assignments of one document from this immutable generation.
    pub fn document_assignments(
        &self,
        document: DocId,
    ) -> impl Iterator<Item = StorageBackendResult<(u32, usize)>> + '_ {
        let mut after = key(document, 0).checked_sub(1);
        let mut done = false;
        std::iter::from_fn(move || {
            if done {
                return None;
            }
            let next = (|| {
                self.control.check()?;
                let Some((found, vector)) = self.vectors.next(after)? else {
                    return Ok(None);
                };
                if vector.doc_id != document {
                    return Ok(None);
                }
                after = Some(found);
                Ok(vector
                    .centroid
                    .map(|centroid| (vector.vector_ordinal, centroid)))
            })();
            match next {
                Ok(Some(assignment)) => Some(Ok(assignment)),
                Ok(None) => {
                    done = true;
                    None
                }
                Err(error) => {
                    done = true;
                    Some(Err(error))
                }
            }
        })
    }

    pub(super) fn insert(
        &mut self,
        document: DocId,
        ordinal: u32,
        raw: &[f32],
        centroid: Option<usize>,
    ) -> StorageBackendResult<()> {
        crate::vector_index::validate_vector_values(self.dimensions, raw)
            .map_err(|error| corrupt_vector("canonical vector", &error))?;
        let bytes = raw
            .len()
            .checked_mul(2 * size_of::<f32>())
            .and_then(|bytes| bytes.checked_add(size_of::<StoredVector>()))
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        let _memory = self.control.memory().reserve(bytes)?;
        let mut vector = raw.to_vec();
        let norm = l2_normalize(&mut vector);
        self.vectors.insert(
            key(document, ordinal),
            StoredVector {
                key: (document, ordinal),
                doc_id: document,
                vector_ordinal: ordinal,
                raw_vector: raw.to_vec(),
                norm,
                vector,
                centroid,
            },
            Some(&self.control),
        )
    }
}

pub(super) fn key(document: DocId, ordinal: u32) -> u128 {
    (u128::from(document) << 32) | u128::from(ordinal)
}
pub(super) fn corrupt(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(format!("corrupt IVF index: {message}"))
}

fn corrupt_vector(kind: &str, error: &StorageBackendError) -> StorageBackendError {
    StorageBackendError::Other(format!("corrupt IVF index: invalid {kind}: {error}"))
}
