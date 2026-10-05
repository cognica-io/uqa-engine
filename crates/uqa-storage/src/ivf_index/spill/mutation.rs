//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated document replay preserves IVF deletion and eager-training rules.

use super::{key, IVFMutation, IVFPreparedMetadata, IVFState};
use crate::{
    ivf_index::math::{l2_normalize, nearest_centroid_controlled},
    StorageBackendError, StorageBackendResult,
};
use uqa_core::{memory::MemoryError, DocId};

impl IVFPreparedMetadata {
    pub(super) fn apply(&mut self, mutation: IVFMutation<'_>) -> StorageBackendResult<()> {
        match mutation {
            IVFMutation::Replace { document, vectors } => {
                for vector in vectors {
                    self.control.check()?;
                    crate::vector_index::validate_vector_values(self.dimensions, vector)?;
                }
                super::super::mutation::validate_vector_ordinal_count(
                    u64::try_from(vectors.len()).unwrap_or(u64::MAX),
                )?;
                self.remove_document(document)?;
                for (ordinal, raw) in vectors.iter().enumerate() {
                    self.control.check()?;
                    self.insert_assigned(document, ordinal as u32, raw)?;
                }
                self.snapshot.vector_count = self.vectors.len();
                if self.snapshot.state == IVFState::Untrained
                    && self.vectors.len() >= self.params.train_threshold
                {
                    self.train()?;
                }
            }
            IVFMutation::Delete(document) => {
                let removed = self.remove_document(document)?;
                if self.snapshot.state != IVFState::Untrained && removed > 0 {
                    self.snapshot.deletes_since_train = self
                        .snapshot
                        .deletes_since_train
                        .checked_add(removed)
                        .ok_or_else(|| {
                            StorageBackendError::Other(
                                "IVF deletes-since-train counter overflow".into(),
                            )
                        })?;
                    if self.snapshot.trained_size > 0
                        && self.snapshot.deletes_since_train
                            > self.snapshot.trained_size / super::super::training::STALE_DENOMINATOR
                    {
                        self.snapshot.state = IVFState::Stale;
                    }
                }
                self.snapshot.vector_count = self.vectors.len();
            }
            IVFMutation::Clear => {
                self.vectors.clear();
                self.snapshot.centroids.clear();
                self.memory = self
                    .memory
                    .split(self.snapshot.centroids.capacity() * size_of::<Vec<f32>>());
                self.snapshot.state = IVFState::Untrained;
                self.snapshot.trained_size = 0;
                self.snapshot.deletes_since_train = 0;
                self.snapshot.vector_count = 0;
            }
            IVFMutation::Train => self.train()?,
        }
        Ok(())
    }

    pub(super) fn insert_assigned(
        &mut self,
        document: DocId,
        ordinal: u32,
        raw: &[f32],
    ) -> StorageBackendResult<()> {
        let _memory = self
            .control
            .memory()
            .reserve(raw.len().checked_mul(4).ok_or(MemoryError::SizeOverflow)?)?;
        let mut normalized = raw.to_vec();
        l2_normalize(&mut normalized);
        let centroid = if self.snapshot.centroids.is_empty() {
            None
        } else {
            Some(nearest_centroid_controlled(
                &normalized,
                &self.snapshot.centroids,
                Some(&self.control),
            )?)
        };
        self.insert(document, ordinal, raw, centroid)
    }

    fn remove_document(&mut self, document: DocId) -> StorageBackendResult<usize> {
        let mut after = key(document, 0).checked_sub(1);
        let mut removed = 0;
        loop {
            self.control.check()?;
            let Some((key, vector)) = self.vectors.next(after)? else {
                break;
            };
            if vector.doc_id != document {
                break;
            }
            drop(vector);
            self.vectors.remove(key, Some(&self.control))?;
            removed += 1;
            after = Some(key);
        }
        Ok(removed)
    }
}
