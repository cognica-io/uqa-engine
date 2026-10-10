//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable IVF readers seek selected posting lists in bounded roots.

use super::{key, IVFPreparedMetadata, IVFState};
use crate::{
    read_control::StorageReadControl,
    spill_map::{Map, Record},
    vector_index::{
        cosine_similarity_with_norms,
        query::{nearest_normalized_centroids, normalized_query, SpillingVectorScores},
        validate_threshold, validate_vector_values, vector_norm, VectorRead,
    },
    ReadOnlySnapshot, StorageBackendError, StorageBackendResult, VectorIndex,
};
use std::{ops::Deref, sync::Arc};
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId, PostingList,
};

pub(crate) struct IVFReadIndex {
    candidate: IVFPreparedMetadata,
    lists: parking_lot::Mutex<Option<Map<VectorAddress>>>,
}

impl Deref for IVFReadIndex {
    type Target = IVFPreparedMetadata;
    fn deref(&self) -> &Self::Target {
        &self.candidate
    }
}

impl IVFReadIndex {
    pub(crate) fn new(candidate: IVFPreparedMetadata) -> StorageBackendResult<Budgeted<Self>> {
        let control = &candidate.control;
        let memory = control.memory().reserve(size_of::<Self>())?;
        Ok(Budgeted::new(
            Self {
                candidate,
                lists: parking_lot::Mutex::new(None),
            },
            memory,
        ))
    }

    /// Posting lists serve ranked reads, so write-only generations do not construct them. Only the generation's own allowance may retain a derived list; independent readers keep their own construction scratch and cancellation.
    fn lists(&self, control: &StorageReadControl) -> StorageBackendResult<Map<VectorAddress>> {
        control.check()?;
        let mut cached = self.lists.lock();
        if let Some(lists) = cached.as_ref() {
            return Ok(lists.clone());
        }
        let mut lists = Map::new(control.memory(), control.memory().limit() / 16);
        let mut after = None;
        let mut position = 0_u64;
        while let Some((found, vector)) = self.vectors.next_with_memory(after, control.memory())? {
            control.check()?;
            if let Some(centroid) = vector.centroid {
                lists.insert(
                    (centroid as u128) << 64 | u128::from(position),
                    VectorAddress(found),
                    Some(control),
                )?;
                position = position
                    .checked_add(1)
                    .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
            }
            after = Some(found);
        }
        control.check()?;
        #[cfg(test)]
        LIST_BUILDS.set(LIST_BUILDS.get() + 1);
        if self.control.memory().shares_allowance(control.memory()) {
            *cached = Some(lists.clone());
        }
        Ok(lists)
    }

    fn top_k(
        &self,
        query: &[f32],
        k: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        control.check()?;
        validate_vector_values(self.dimensions, query)?;
        if k == 0 {
            return Ok(PostingList::new());
        }
        if self.snapshot.state == IVFState::Stale {
            let candidate = self
                .clone_controlled(control)?
                .prepare(&[super::IVFMutation::Train])?;
            return Self::new(candidate)?.top_k(query, k, control);
        }
        let (normalized, norm) = normalized_query(query, Some(control))?;
        let mut scores = SpillingVectorScores::new(control);
        if self.snapshot.centroids.is_empty() {
            self.scan_scores(query, norm, None, &mut scores, control)?;
        } else {
            let lists = self.lists(control)?;
            let probes = nearest_normalized_centroids(
                &normalized,
                &self.snapshot.centroids,
                self.params.nprobe,
                Some(control),
            )?;
            for &centroid in probes.iter() {
                let prefix = (centroid as u128) << 64;
                let mut after = prefix.checked_sub(1);
                loop {
                    control.check()?;
                    let Some((found, address)) = lists.next_with_memory(after, control.memory())?
                    else {
                        break;
                    };
                    if found >> 64 != centroid as u128 {
                        break;
                    }
                    let vector = self
                        .vectors
                        .get_with_memory(address.0, control.memory())?
                        .ok_or_else(|| super::corrupt("posting references absent vector"))?;
                    scores.add(
                        vector.doc_id,
                        cosine_similarity_with_norms(query, &vector.raw_vector, norm, vector.norm),
                    )?;
                    after = Some(found);
                }
            }
        }
        scores.finish(k)
    }

    fn scan_scores(
        &self,
        query: &[f32],
        norm: f32,
        threshold: Option<f32>,
        scores: &mut SpillingVectorScores,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let mut after = None;
        loop {
            control.check()?;
            let Some((key, vector)) = self.vectors.next_with_memory(after, control.memory())?
            else {
                break;
            };
            let similarity =
                cosine_similarity_with_norms(query, &vector.raw_vector, norm, vector.norm);
            if threshold.is_none_or(|threshold| similarity >= threshold) {
                scores.add(vector.doc_id, similarity)?;
            }
            after = Some(key);
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static LIST_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;

impl VectorIndex for IVFReadIndex {
    fn contains_document(&self, document: DocId) -> StorageBackendResult<bool> {
        self.vectors.contains_key(key(document, 0))
    }
    fn initialize(&mut self) -> StorageBackendResult<()> {
        Err(read_only())
    }

    fn dimensions(&self) -> u32 {
        self.dimensions
    }
    fn index_kind(&self) -> &'static str {
        "ivf"
    }
    fn add(&mut self, _: DocId, _: Vec<f32>) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn add_many(&mut self, _: DocId, _: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.top_k(query, k, &self.control)
    }
    fn search_knn_with_control(
        &self,
        query: &[f32],
        k: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.top_k(query, k, control)
    }
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.search_threshold_with_control(query, threshold, &self.control)
    }
    fn search_threshold_with_control(
        &self,
        query: &[f32],
        threshold: f32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        control.check()?;
        validate_vector_values(self.dimensions, query)?;
        validate_threshold(threshold)?;
        let mut scores = SpillingVectorScores::new(control);
        self.scan_scores(
            query,
            vector_norm(query),
            Some(threshold),
            &mut scores,
            control,
        )?;
        scores.finish(usize::MAX)
    }
    fn count(&self) -> StorageBackendResult<usize> {
        Ok(self.snapshot.vector_count)
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        self.snapshot_with_control(&self.control)
    }
    fn snapshot_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        ReadOnlySnapshot::from_budgeted(Self::new(self.clone_controlled(control)?)?)?
            .with_canonical_vectors(Some(control))?
            .with_vector_read_control(control)?
            .snapshot()
    }
}

impl VectorRead for IVFReadIndex {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        control.check()
    }
    fn dimensions(&self) -> u32 {
        self.dimensions
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.candidate.next_document_after(after, control)
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.candidate.document_vector_count(document, control)
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        self.candidate.read_vector(document, ordinal, control)
    }
}

fn read_only() -> StorageBackendError {
    StorageBackendError::Other("cannot mutate immutable IVF reader".into())
}

struct VectorAddress(u128);
impl Record for VectorAddress {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        Ok(size_of::<Self>())
    }
    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        Ok(16)
    }
    fn encode(&self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(&self.0.to_le_bytes());
    }
    fn decode(bytes: &[u8]) -> StorageBackendResult<Self> {
        Ok(Self(u128::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| super::corrupt("vector address width"))?,
        )))
    }
}
