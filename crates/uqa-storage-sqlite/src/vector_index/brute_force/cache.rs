//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optional canonical-vector reuse retains the original allowance and scores each current view.

use super::{BudgetedVec, DocId, NativeVectorRead, SQLiteError, SQLiteResult, SQLiteVectorIndex};
use crate::mvcc::native::NativeRecordFamily as Family;
use crate::vector_index::native::{
    identity::{identity, VectorIdentity},
    VectorBuffer,
};
use uqa_core::memory::MemoryError;
use uqa_storage::{ReadOnlySnapshot, StorageBackendError};

type Coordinates = Vec<(DocId, Vec<f32>)>;

pub(super) struct CachedVectors {
    identity: VectorIdentity,
    coordinates: ReadOnlySnapshot<Coordinates>,
}

fn append(
    buffer: &mut VectorBuffer<(DocId, Vec<f32>)>,
    id: DocId,
    values: &[f32],
) -> Result<(), MemoryError> {
    buffer.rows.reserve(1)?;
    let mut vector = BudgetedVec::new(buffer.rows.budget());
    vector.extend_from_slice(values)?;
    let (vector, memory) = vector.into_parts();
    buffer.rows.push((id, vector))?;
    buffer.payload.absorb(memory);
    Ok(())
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
        let cached = self
            .cached_vectors
            .read()
            .as_ref()
            .filter(|cached| cached.identity == identity)
            .map(|cached| cached.coordinates.clone());
        if let Some(cached) = cached {
            let result = Self::score_stream(query, threshold, |visit| {
                for (id, vector) in cached.iter() {
                    read.snapshot.control.check()?;
                    visit(*id, vector, &read.snapshot.control)?;
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
        let mut pending = Some(VectorBuffer::with_memory(&cache_memory));
        let result = Self::score_stream(query, threshold, |visit| {
            read.visit_vectors(|id, vector, control| {
                visit(id, vector, control)?;
                if pending
                    .as_mut()
                    .is_some_and(|buffer| append(buffer, id, vector).is_err())
                {
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
            match ReadOnlySnapshot::from_budgeted(pending.finish()) {
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
