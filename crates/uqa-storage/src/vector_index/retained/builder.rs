//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated vectors move into one charged immutable corpus before publication.

use super::{RetainedVectorIndex, VectorEntries};
use crate::{
    read_control::StorageReadControl, vector_index::validate_vector_values, StorageBackendError,
    StorageBackendResult,
};
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryError, MemoryReservation},
    ordering::sort_by_with_control,
    DocId,
};

/// Append selected document tensors without copying their buffers or reserving their payload twice. The caller converts inputs under this builder's allowance and transfers their reservations. Finalization orders identities, validates unique tensor ordinals and retains the same allowance for readers.
pub struct RetainedVectorIndexBuilder {
    entries: BudgetedVec<(DocId, u32, Vec<f32>)>,
    payload: MemoryReservation,
    dimensions: u32,
    control: StorageReadControl,
}

impl RetainedVectorIndexBuilder {
    pub fn new(dimensions: u32, control: &StorageReadControl) -> Self {
        Self {
            entries: BudgetedVec::new(control.memory()),
            payload: control.memory().empty_reservation(),
            dimensions,
            control: control.clone(),
        }
    }

    pub fn dimensions(&self) -> u32 {
        self.dimensions
    }

    pub fn add_document(
        &mut self,
        id: DocId,
        vectors: Budgeted<Vec<Vec<f32>>>,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        u32::try_from(vectors.len().saturating_sub(1)).map_err(|_| {
            StorageBackendError::Other("vector ordinal exceeds u32 index format".into())
        })?;
        // Drop unadopted vector buffers before releasing their reservation on every error.
        let mut pending = vectors.into_parts();
        if !pending.1.budget().shares_allowance(self.control.memory()) {
            return Err(StorageBackendError::Other(
                "retained vectors must use the original index allowance".into(),
            ));
        }
        let mut bytes = 0_usize;
        for vector in &pending.0 {
            self.control.check()?;
            bytes = vector
                .capacity()
                .checked_mul(size_of::<f32>())
                .and_then(|payload| bytes.checked_add(payload))
                .ok_or(MemoryError::SizeOverflow)?;
        }
        let required = pending
            .0
            .capacity()
            .checked_mul(size_of::<Vec<f32>>())
            .and_then(|headers| headers.checked_add(bytes))
            .ok_or(MemoryError::SizeOverflow)?;
        if pending.1.bytes() < required {
            return Err(StorageBackendError::Other(
                "retained vector reservation does not cover its buffers".into(),
            ));
        }
        for vector in &pending.0 {
            self.control.check()?;
            validate_vector_values(self.dimensions, vector)?;
        }
        self.entries.reserve(pending.0.len())?;
        let original = self.entries.len();
        let append = (|| -> StorageBackendResult<()> {
            for (ordinal, vector) in pending.0.drain(..).enumerate() {
                self.control.check()?;
                let ordinal = u32::try_from(ordinal).expect("validated vector ordinal count");
                self.entries.push((id, ordinal, vector))?;
            }
            self.control.check()
        })();
        if let Err(error) = append {
            self.entries.truncate(original);
            return Err(error);
        }
        self.payload.absorb(pending.1.split(bytes));
        // The now-empty outer buffer drops with the rest of its reservation.
        Ok(())
    }

    pub fn finish(mut self) -> StorageBackendResult<RetainedVectorIndex> {
        sort_by_with_control(
            &mut self.entries,
            &mut || self.control.check(),
            |left, right, _| Ok((left.0, left.1).cmp(&(right.0, right.1))),
        )?;
        let mut previous: Option<(DocId, u32)> = None;
        for (id, ordinal, _) in self.entries.iter() {
            self.control.check()?;
            let expected = match previous {
                Some((previous_id, previous_ordinal)) if previous_id == *id => {
                    previous_ordinal.checked_add(1)
                }
                _ => Some(0),
            };
            if expected != Some(*ordinal) {
                return Err(StorageBackendError::Other(
                    "retained vector input repeats a document or tensor ordinal".into(),
                ));
            }
            previous = Some((*id, *ordinal));
        }
        let (entries, mut memory): (VectorEntries, _) = self.entries.into_parts();
        memory.absorb(self.payload);
        RetainedVectorIndex::from_entries(
            Budgeted::new(entries, memory),
            self.dimensions,
            "memory-bruteforce",
            &self.control,
        )
    }
}
