//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optional canonical-vector reuse retains the original allowance and scores each current view.

use super::{BudgetedVec, DocId, NativeVectorRead, SQLiteError, SQLiteResult, SQLiteVectorIndex};
use crate::mvcc::native::NativeRecordFamily as Family;
use crate::vector_index::native::identity::{identity, VectorIdentity};
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError};
use uqa_storage::{
    vector_index::{cosine_similarity_with_norms, vector_norm},
    ReadOnlySnapshot, StorageBackendError,
};

struct Coordinates {
    rows: Vec<(DocId, f32)>,
    values: Vec<f32>,
}

struct PendingCoordinates {
    rows: BudgetedVec<(DocId, f32)>,
    values: BudgetedVec<f32>,
}

impl PendingCoordinates {
    fn new(memory: &MemoryBudget) -> Self {
        Self {
            rows: BudgetedVec::new(memory),
            values: BudgetedVec::new(memory),
        }
    }

    fn append(
        &mut self,
        id: DocId,
        norm: f32,
        values: &[f32],
        limit: usize,
    ) -> Result<(), MemoryError> {
        let rows = self.rows.len().saturating_add(1);
        let floats = self.values.len().saturating_add(values.len());
        let required = Self::bytes(rows, floats);
        if required > limit {
            return Err(MemoryError::Limit { required, limit });
        }
        self.rows.reserve(1)?;
        self.values.extend_from_slice(values)?;
        self.rows.push((id, norm))
    }

    fn bytes(rows: usize, floats: usize) -> usize {
        rows.saturating_mul(size_of::<(DocId, f32)>())
            .saturating_add(floats.saturating_mul(size_of::<f32>()))
            .saturating_add(size_of::<Coordinates>())
            .saturating_add(size_of::<uqa_core::memory::MemoryReservation>())
    }

    fn finish(
        mut self,
        cache: &MemoryBudget,
    ) -> Result<ReadOnlySnapshot<Coordinates>, StorageBackendError> {
        // Replacement buffers belong to query workspace. Only the immutable buffers and their shared handle survive in the cache.
        if Self::bytes(self.rows.capacity(), self.values.capacity()) > cache.limit() {
            self.rows.shrink_to_fit()?;
            self.values.shrink_to_fit()?;
        }
        let (rows, mut memory) = self.rows.into_parts();
        let (values, coordinates) = self.values.into_parts();
        memory.absorb(coordinates);
        memory.restrict_to_child(cache)?;
        ReadOnlySnapshot::from_budgeted(Budgeted::new(Coordinates { rows, values }, memory))
    }
}

pub(super) struct CachedVectors {
    identity: VectorIdentity,
    coordinates: ReadOnlySnapshot<Coordinates>,
}

impl SQLiteVectorIndex {
    pub(super) fn read_releasing_cache<T>(
        &self,
        mut read: impl FnMut() -> SQLiteResult<T>,
    ) -> SQLiteResult<T> {
        match read() {
            Err(SQLiteError::Memory(error)) => {
                if self.cached_vectors.write().take().is_some() {
                    read()
                } else {
                    Err(SQLiteError::Memory(error))
                }
            }
            result => result,
        }
    }

    pub(super) fn score_cached_vectors(
        &self,
        read: &NativeVectorRead<'_>,
        query: &[f32],
        threshold: Option<f32>,
    ) -> SQLiteResult<Option<BudgetedVec<(DocId, f32)>>> {
        let selected = match self.read_releasing_cache(|| identity(read, &[Family::Vectors])) {
            Err(SQLiteError::Memory(_)) => {
                return Self::score_stream(query, threshold, |visit| read.visit_vectors(visit));
            }
            result => result?,
        };
        let Some(identity) = selected else {
            self.cached_vectors.write().take();
            return Self::score_stream(query, threshold, |visit| read.visit_vectors(visit));
        };
        let query_norm = vector_norm(query);
        let cached = self
            .cached_vectors
            .read()
            .as_ref()
            .filter(|cached| cached.identity == identity)
            .map(|cached| cached.coordinates.clone());
        if let Some(cached) = cached {
            let result = Self::collect_scores(threshold, |emit| {
                let mut values = &cached.values[..];
                for &(id, norm) in &cached.rows {
                    read.snapshot.control.check()?;
                    let (vector, rest) = values.split_at(self.dimensions as usize);
                    values = rest;
                    emit(
                        id,
                        cosine_similarity_with_norms(query, vector, query_norm, norm),
                        &read.snapshot.control,
                    )?;
                }
                read.snapshot.control.check()?;
                Ok(())
            });
            drop(cached);
            if !matches!(result, Err(SQLiteError::Memory(_))) {
                return result;
            }
            self.cached_vectors.write().take();
            return Self::score_stream(query, threshold, |visit| read.visit_vectors(visit));
        }
        // Release the replaceable generation before admitting another one. Active readers retain their own shared allocation leases.
        self.cached_vectors.write().take();
        let memory = read.snapshot.control.memory();
        let cache_memory = memory.child((memory.limit() / 16).min(16 * 1024 * 1024));
        let mut pending = Some(PendingCoordinates::new(memory));
        let result = Self::collect_scores(threshold, |emit| {
            read.visit_vectors(|id, vector, control| {
                let norm = vector_norm(vector);
                emit(
                    id,
                    cosine_similarity_with_norms(query, vector, query_norm, norm),
                    control,
                )?;
                if pending.as_mut().is_some_and(|buffer| {
                    buffer
                        .append(id, norm, vector, cache_memory.limit())
                        .is_err()
                }) {
                    pending = None;
                }
                Ok(())
            })
        });
        if matches!(result, Err(SQLiteError::Memory(_))) && pending.is_some() {
            // A partially retained corpus can compete with later source or score workspace. Discard both partial products and score the same immutable view once without retention; no SQL, analysis or mutation is replayed.
            drop(pending);
            return Self::score_stream(query, threshold, |visit| read.visit_vectors(visit));
        }
        let scores = result?;
        if let Some(pending) = pending {
            read.snapshot.control.check()?;
            match pending.finish(&cache_memory) {
                Ok(coordinates) => {
                    *self.cached_vectors.write() = Some(CachedVectors {
                        identity,
                        coordinates,
                    });
                }
                Err(StorageBackendError::Memory(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(scores)
    }
}

#[cfg(test)]
mod tests;
