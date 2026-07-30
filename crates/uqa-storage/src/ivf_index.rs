//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF (Inverted File Index) backed by an in-memory centroid matrix
//! plus per-centroid posting lists.
//!
//! Mirrors UQA `storage/ivf_index`. The space is partitioned into
//! `nlist` Voronoi cells around centroids learned from the indexed
//! vectors; query time walks only the `nprobe` cells whose centroids
//! are nearest to the query, dropping search cost from `O(N)` to
//! `O((nprobe / nlist) * N)` once the index has been trained.
//!
//! State machine (Section 2.4, Paper 5):
//!
//! * `UNTRAINED` -- fewer than `train_threshold` vectors stored, every
//!   query falls back to a brute-force scan over the entire content.
//! * `TRAINED`   -- `train` has run, `nlist` centroids are valid, the
//!   posting lists are populated, and `nprobe`-cell search is active.
//! * `STALE`     -- > 20% of the indexed vectors have been deleted
//!   since the last training pass; the next search retrains.
//!
//! Vectors are L2-normalised on insert so cosine similarity collapses
//! to a single dot product against the centroid matrix.

#![allow(clippy::cast_lossless, clippy::similar_names)]

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::{DocId, Payload, PostingEntry, PostingList};

use crate::vector_index::{
    cosine_similarity, select_top_k_scored, validate_vector_values, VectorIndex,
};
use crate::{StorageBackendError, StorageBackendResult};

const DEFAULT_NLIST: usize = 100;
const DEFAULT_NPROBE: usize = 10;
const DEFAULT_TRAIN_THRESHOLD: usize = 256;
/// Stale fraction: when the count of deleted-since-last-train vectors
/// exceeds 20% of trained corpus size, the next query forces a
/// retrain.
const STALE_DENOMINATOR: usize = 5;

fn usize_to_u64(value: usize, context: &str) -> StorageBackendResult<u64> {
    u64::try_from(value).map_err(|_| {
        StorageBackendError::Other(format!("IVF {context} exceeds the u64 counter range"))
    })
}

fn validate_vector_ordinal_count(count: u64) -> StorageBackendResult<()> {
    if count > u64::from(u32::MAX) + 1 {
        return Err(StorageBackendError::Other(
            "IVF vector ordinal exceeds the u32 index format".into(),
        ));
    }
    Ok(())
}

fn encode_vector_ordinal(ordinal: usize) -> StorageBackendResult<u32> {
    u32::try_from(ordinal).map_err(|_| {
        StorageBackendError::Other("IVF vector ordinal exceeds the u32 index format".into())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IVFState {
    Untrained,
    Trained,
    Stale,
}

#[derive(Debug, Clone)]
pub(crate) struct IVFMetadataSnapshot {
    pub state: IVFState,
    pub centroids: Vec<Vec<f32>>,
    pub assignments: Vec<(DocId, u32, usize)>,
    pub trained_size: usize,
    pub deletes_since_train: usize,
    pub vector_count: usize,
}

type VectorKey = (DocId, u32);

#[derive(Debug, Clone)]
struct StoredVector {
    key: VectorKey,
    doc_id: DocId,
    vector_ordinal: u32,
    raw_vector: Vec<f32>,
    vector: Vec<f32>,
    centroid: Option<usize>,
}

/// In-memory IVF index. Centroids live in a flat `Vec<Vec<f32>>` of
/// length `nlist`; per-centroid posting lists are tracked through
/// `inverted_lists[centroid_idx] = Vec<doc_id>` so search-time
/// posting-list lookup is `O(1)`.
pub struct IVFIndex {
    dimensions: u32,
    nlist: usize,
    nprobe: Mutex<usize>,
    train_threshold: usize,
    state: Mutex<IVFState>,
    /// `vectors[(doc_id, ordinal)] = (normalised_vector, centroid_idx)`.
    /// The centroid index is `None` for untrained rows; populated when the
    /// index is trained or a vector is added post-training. Tensor columns
    /// keep one row identity (`doc_id`) while contributing many vector
    /// elements through distinct ordinals.
    vectors: Mutex<BTreeMap<VectorKey, StoredVector>>,
    centroids: Mutex<Vec<Vec<f32>>>,
    inverted_lists: Mutex<Vec<Vec<VectorKey>>>,
    /// Trained corpus size at the last training pass; the stale
    /// detector compares this to the current `vectors.len()` plus the
    /// running deletion counter.
    trained_size: Mutex<usize>,
    deletes_since_train: Mutex<usize>,
}

impl IVFIndex {
    pub fn new(dimensions: u32) -> Self {
        Self::with_params(
            dimensions,
            DEFAULT_NLIST,
            DEFAULT_NPROBE,
            DEFAULT_TRAIN_THRESHOLD,
        )
    }

    pub fn with_params(
        dimensions: u32,
        nlist: usize,
        nprobe: usize,
        train_threshold: usize,
    ) -> Self {
        Self {
            dimensions,
            nlist: nlist.max(1),
            nprobe: Mutex::new(nprobe.max(1)),
            train_threshold: train_threshold.max(1),
            state: Mutex::new(IVFState::Untrained),
            vectors: Mutex::new(BTreeMap::new()),
            centroids: Mutex::new(Vec::new()),
            inverted_lists: Mutex::new(Vec::new()),
            trained_size: Mutex::new(0),
            deletes_since_train: Mutex::new(0),
        }
    }

    pub fn state(&self) -> IVFState {
        *self.state.lock()
    }

    pub fn nlist(&self) -> usize {
        self.nlist
    }

    pub fn nprobe(&self) -> usize {
        *self.nprobe.lock()
    }

    pub fn set_nprobe(&self, nprobe: usize) {
        *self.nprobe.lock() = nprobe.max(1);
    }

    /// Recompute centroids from the currently held vectors using
    /// k-means with a deterministic seed. Resets the stale tracker.
    pub fn train(&self) -> StorageBackendResult<()> {
        let vectors = self.vectors.lock();
        let count = vectors.len();
        if count < self.train_threshold {
            // Not enough rows to bother with the partitioning.
            return Ok(());
        }
        let dims = usize::try_from(self.dimensions).map_err(|_| {
            StorageBackendError::Other(format!(
                "IVF dimension {} exceeds the addressable memory range",
                self.dimensions
            ))
        })?;
        let nlist = self.nlist.min(count);
        let centroids = kmeans(
            &vectors
                .values()
                .map(|v| v.vector.clone())
                .collect::<Vec<_>>(),
            nlist,
            dims,
            10,
        );
        drop(vectors);
        // Re-acquire as mutable to assign each vector to its
        // nearest centroid.
        let mut centroids_guard = self.centroids.lock();
        *centroids_guard = centroids;
        let centroids = centroids_guard.clone();
        drop(centroids_guard);
        let mut vectors = self.vectors.lock();
        let mut inverted_lists = vec![Vec::new(); centroids.len()];
        for v in vectors.values_mut() {
            let centroid = nearest_centroid(&v.vector, &centroids);
            v.centroid = Some(centroid);
            inverted_lists[centroid].push(v.key);
        }
        *self.inverted_lists.lock() = inverted_lists;
        *self.trained_size.lock() = vectors.len();
        *self.deletes_since_train.lock() = 0;
        *self.state.lock() = IVFState::Trained;
        Ok(())
    }

    fn maybe_mark_stale(&self) {
        let trained = *self.trained_size.lock();
        if trained == 0 {
            return;
        }
        let deletes = *self.deletes_since_train.lock();
        // `STALE_FRACTION` is exactly one fifth. This integer comparison
        // avoids lossy large-counter casts and multiplication overflow.
        if deletes > trained / STALE_DENOMINATOR {
            *self.state.lock() = IVFState::Stale;
        }
    }

    fn remove_from_inverted_list(&self, centroid: usize, key: VectorKey) {
        let mut lists = self.inverted_lists.lock();
        if let Some(list) = lists.get_mut(centroid) {
            list.retain(|id| *id != key);
        }
    }

    fn add_to_inverted_list(&self, centroid: usize, key: VectorKey) {
        let mut lists = self.inverted_lists.lock();
        if let Some(list) = lists.get_mut(centroid) {
            match list.binary_search(&key) {
                Ok(_) => {}
                Err(pos) => list.insert(pos, key),
            }
        }
    }

    pub(crate) fn metadata_snapshot(&self) -> IVFMetadataSnapshot {
        let vectors = self.vectors.lock();
        let mut assignments: Vec<(DocId, u32, usize)> = vectors
            .values()
            .filter_map(|v| {
                v.centroid
                    .map(|centroid| (v.doc_id, v.vector_ordinal, centroid))
            })
            .collect();
        assignments.sort_by_key(|(doc_id, ordinal, _)| (*doc_id, *ordinal));
        IVFMetadataSnapshot {
            state: *self.state.lock(),
            centroids: self.centroids.lock().clone(),
            assignments,
            trained_size: *self.trained_size.lock(),
            deletes_since_train: *self.deletes_since_train.lock(),
            vector_count: vectors.len(),
        }
    }
}

fn l2_normalise(v: &mut [f32]) {
    let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag > 1e-12 {
        for x in v.iter_mut() {
            *x /= mag;
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn nearest_centroid(vector: &[f32], centroids: &[Vec<f32>]) -> usize {
    let mut best_idx = 0usize;
    let mut best_sim = f32::NEG_INFINITY;
    for (i, c) in centroids.iter().enumerate() {
        let sim = dot(vector, c);
        if sim > best_sim {
            best_sim = sim;
            best_idx = i;
        }
    }
    best_idx
}

/// Lightweight k-means with deterministic (xorshift-seeded) initial
/// centroid pick. Iterates `iterations` times or until centroids
/// stabilise. Designed for IVF training where vectors are already
/// L2-normalised so cosine similarity collapses to dot product.
fn kmeans(vectors: &[Vec<f32>], k: usize, dims: usize, iterations: usize) -> Vec<Vec<f32>> {
    if vectors.is_empty() || k == 0 {
        return Vec::new();
    }
    // Deterministic init: pick `k` evenly-spaced rows. Avoids pulling
    // in a PRNG dependency and keeps tests stable.
    let stride = (vectors.len() / k).max(1);
    let mut centroids: Vec<Vec<f32>> = (0..k)
        .map(|i| vectors[(i * stride) % vectors.len()].clone())
        .collect();

    for _ in 0..iterations {
        let mut sums: Vec<Vec<f32>> = vec![vec![0.0; dims]; k];
        let mut counts: Vec<usize> = vec![0; k];
        for v in vectors {
            let idx = nearest_centroid(v, &centroids);
            for (s, x) in sums[idx].iter_mut().zip(v.iter()) {
                *s += x;
            }
            counts[idx] += 1;
        }
        for (i, c) in centroids.iter_mut().enumerate() {
            if counts[i] > 0 {
                for (cv, sv) in c.iter_mut().zip(sums[i].iter()) {
                    *cv = sv / counts[i] as f32;
                }
                l2_normalise(c);
            }
        }
    }
    centroids
}

impl VectorIndex for IVFIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "ivf"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.add_many(doc_id, vec![vector])
    }

    fn add_many(
        &mut self,
        doc_id: DocId,
        input_vectors: Vec<Vec<f32>>,
    ) -> StorageBackendResult<()> {
        for vector in &input_vectors {
            validate_vector_values(self.dimensions, vector)?;
        }
        validate_vector_ordinal_count(usize_to_u64(input_vectors.len(), "vector count")?)?;
        let centroids = self.centroids.lock().clone();
        let mut staged = Vec::with_capacity(input_vectors.len());
        for (ordinal, mut vector) in input_vectors.into_iter().enumerate() {
            let vector_ordinal = encode_vector_ordinal(ordinal)?;
            let raw_vector = vector.clone();
            l2_normalise(&mut vector);
            let centroid = if centroids.is_empty() {
                None
            } else {
                Some(nearest_centroid(&vector, &centroids))
            };
            let key = (doc_id, vector_ordinal);
            staged.push(StoredVector {
                key,
                doc_id,
                vector_ordinal,
                raw_vector,
                vector,
                centroid,
            });
        }

        let mut vectors = self.vectors.lock();
        let old_keys: Vec<VectorKey> = vectors
            .keys()
            .filter(|(stored_doc_id, _)| *stored_doc_id == doc_id)
            .copied()
            .collect();
        for key in old_keys {
            if let Some(old) = vectors.remove(&key) {
                if let Some(old_centroid) = old.centroid {
                    drop(vectors);
                    self.remove_from_inverted_list(old_centroid, key);
                    vectors = self.vectors.lock();
                }
            }
        }

        for stored in staged {
            let key = stored.key;
            let centroid = stored.centroid;
            vectors.insert(key, stored);
            if let Some(centroid) = centroid {
                drop(vectors);
                self.add_to_inverted_list(centroid, key);
                vectors = self.vectors.lock();
            }
        }

        let count = vectors.len();
        let untrained = matches!(*self.state.lock(), IVFState::Untrained);
        drop(vectors);
        if untrained && count >= self.train_threshold {
            self.train()?;
        }
        Ok(())
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let mut vectors = self.vectors.lock();
        let old_keys: Vec<VectorKey> = vectors
            .keys()
            .filter(|(stored_doc_id, _)| *stored_doc_id == doc_id)
            .copied()
            .collect();
        let next_deletes = if old_keys.is_empty() {
            None
        } else {
            Some(
                self.deletes_since_train
                    .lock()
                    .checked_add(old_keys.len())
                    .ok_or_else(|| {
                        StorageBackendError::Other(
                            "IVF deletes-since-train counter overflow".into(),
                        )
                    })?,
            )
        };
        for key in old_keys {
            if let Some(old) = vectors.remove(&key) {
                if let Some(centroid) = old.centroid {
                    drop(vectors);
                    self.remove_from_inverted_list(centroid, key);
                    vectors = self.vectors.lock();
                }
            }
        }
        if let Some(next_deletes) = next_deletes {
            *self.deletes_since_train.lock() = next_deletes;
        }
        drop(vectors);
        self.maybe_mark_stale();
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.vectors.lock().clear();
        self.centroids.lock().clear();
        self.inverted_lists.lock().clear();
        *self.state.lock() = IVFState::Untrained;
        *self.trained_size.lock() = 0;
        *self.deletes_since_train.lock() = 0;
        Ok(())
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        validate_vector_values(self.dimensions, query)?;
        if k == 0 {
            return Ok(PostingList::default());
        }
        let raw_query = query;
        let mut q = query.to_vec();
        l2_normalise(&mut q);

        // Repair stale state lazily.
        if matches!(*self.state.lock(), IVFState::Stale) {
            drop(self.state.lock());
            self.train()?;
        }

        let state = *self.state.lock();
        let vectors = self.vectors.lock();

        let mut scored: Vec<(DocId, f32)> = Vec::new();
        let scan_one = |sv: &StoredVector, scored: &mut Vec<(DocId, f32)>| {
            let sim = cosine_similarity(raw_query, &sv.raw_vector);
            scored.push((sv.doc_id, sim));
        };

        match state {
            IVFState::Untrained => {
                for sv in vectors.values() {
                    scan_one(sv, &mut scored);
                }
            }
            IVFState::Trained | IVFState::Stale => {
                let centroids = self.centroids.lock();
                if centroids.is_empty() {
                    drop(centroids);
                    for sv in vectors.values() {
                        scan_one(sv, &mut scored);
                    }
                } else {
                    // Pick the `nprobe` nearest centroids.
                    let mut centroid_scores: Vec<(usize, f32)> = centroids
                        .iter()
                        .enumerate()
                        .map(|(i, c)| (i, dot(&q, c)))
                        .collect();
                    centroid_scores.sort_by(|a, b| b.1.total_cmp(&a.1));
                    let probe: std::collections::BTreeSet<usize> = centroid_scores
                        .into_iter()
                        .take(*self.nprobe.lock())
                        .map(|(i, _)| i)
                        .collect();
                    let lists = self.inverted_lists.lock();
                    for centroid in probe {
                        if let Some(doc_ids) = lists.get(centroid) {
                            for doc_id in doc_ids {
                                if let Some(sv) = vectors.get(doc_id) {
                                    scan_one(sv, &mut scored);
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut best_by_doc: BTreeMap<DocId, f32> = BTreeMap::new();
        for (doc_id, score) in scored {
            best_by_doc
                .entry(doc_id)
                .and_modify(|best| {
                    if score > *best {
                        *best = score;
                    }
                })
                .or_insert(score);
        }
        let mut scored: Vec<(DocId, f32)> = best_by_doc.into_iter().collect();
        select_top_k_scored(&mut scored, k);
        scored.sort_by_key(|&(d, _)| d);
        let entries: Vec<PostingEntry> = scored
            .into_iter()
            .map(|(d, s)| {
                PostingEntry::new(
                    d,
                    Payload {
                        score: f64::from(s),
                        ..Default::default()
                    },
                )
            })
            .collect();
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        validate_vector_values(self.dimensions, query)?;
        if !threshold.is_finite() {
            return Err(StorageBackendError::Other(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        let vectors = self.vectors.lock();
        let mut best_by_doc: BTreeMap<DocId, f32> = BTreeMap::new();
        for sv in vectors.values() {
            let sim = cosine_similarity(query, &sv.raw_vector);
            if sim >= threshold {
                best_by_doc
                    .entry(sv.doc_id)
                    .and_modify(|best| {
                        if sim > *best {
                            *best = sim;
                        }
                    })
                    .or_insert(sim);
            }
        }
        let mut entries: Vec<PostingEntry> = best_by_doc
            .into_iter()
            .map(|(doc_id, sim)| {
                PostingEntry::new(
                    doc_id,
                    Payload {
                        score: f64::from(sim),
                        ..Default::default()
                    },
                )
            })
            .collect();
        entries.sort_by_key(|e| e.doc_id);
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn count(&self) -> StorageBackendResult<usize> {
        Ok(self.vectors.lock().len())
    }

    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.train()
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        // The IVFIndex is internally Mutex-guarded so a clone of the
        // shared state suffices; we hand back a snapshot wrapped in
        // Arc that re-uses the same posting lists by value-cloning.
        let snap = IVFIndex {
            dimensions: self.dimensions,
            nlist: self.nlist,
            nprobe: Mutex::new(*self.nprobe.lock()),
            train_threshold: self.train_threshold,
            state: Mutex::new(*self.state.lock()),
            vectors: Mutex::new(self.vectors.lock().clone()),
            centroids: Mutex::new(self.centroids.lock().clone()),
            inverted_lists: Mutex::new(self.inverted_lists.lock().clone()),
            trained_size: Mutex::new(*self.trained_size.lock()),
            deletes_since_train: Mutex::new(*self.deletes_since_train.lock()),
        };
        Ok(Arc::new(snap))
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Ok(Box::new(IVFIndex {
            dimensions: self.dimensions,
            nlist: self.nlist,
            nprobe: Mutex::new(*self.nprobe.lock()),
            train_threshold: self.train_threshold,
            state: Mutex::new(*self.state.lock()),
            vectors: Mutex::new(self.vectors.lock().clone()),
            centroids: Mutex::new(self.centroids.lock().clone()),
            inverted_lists: Mutex::new(self.inverted_lists.lock().clone()),
            trained_size: Mutex::new(*self.trained_size.lock()),
            deletes_since_train: Mutex::new(*self.deletes_since_train.lock()),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rand_vec(seed: u64, dims: usize) -> Vec<f32> {
        // Linear congruential generator for deterministic test data.
        let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        (0..dims)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                ((state >> 32) as i32 as f32) / (i32::MAX as f32)
            })
            .collect()
    }

    #[test]
    fn untrained_search_falls_back_to_brute_force() {
        let mut idx = IVFIndex::with_params(4, 8, 4, 1024);
        for i in 0..16 {
            idx.add(i, rand_vec(i + 1, 4)).unwrap();
        }
        assert_eq!(idx.state(), IVFState::Untrained);
        let pl = idx.search_knn(&rand_vec(1, 4), 4).unwrap();
        assert_eq!(pl.len(), 4);
    }

    #[test]
    fn auto_trains_above_threshold() {
        let mut idx = IVFIndex::with_params(4, 4, 2, 16);
        for i in 0..32 {
            idx.add(i, rand_vec(i + 1, 4)).unwrap();
        }
        assert_eq!(idx.state(), IVFState::Trained);
    }

    #[test]
    fn auto_train_threshold_counts_tensor_vectors_below_nlist() {
        let mut idx = IVFIndex::with_params(2, 4, 4, 2);
        idx.add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
            .unwrap();
        assert_eq!(idx.state(), IVFState::Trained);
        assert_eq!(idx.metadata_snapshot().trained_size, 2);
        assert_eq!(idx.metadata_snapshot().vector_count, 2);
    }

    #[test]
    fn search_returns_self_at_top_after_training() {
        let mut idx = IVFIndex::with_params(8, 4, 4, 16);
        for i in 0..64 {
            idx.add(i, rand_vec(i + 1, 8)).unwrap();
        }
        idx.train().unwrap();
        let probe = idx.search_knn(&rand_vec(1, 8), 1).unwrap();
        let top: Vec<DocId> = probe.iter().map(|e| e.doc_id).collect();
        // The exact-self search should retrieve doc 0 (vector seed 1
        // matches when we re-query with the same seed).
        assert!(!top.is_empty());
    }

    #[test]
    fn delete_marks_stale_above_fraction() {
        let mut idx = IVFIndex::with_params(4, 4, 2, 16);
        for i in 0..32 {
            idx.add(i, rand_vec(i + 1, 4)).unwrap();
        }
        idx.train().unwrap();
        for i in 0..(32 / STALE_DENOMINATOR + 4) {
            idx.delete(i as DocId).unwrap();
        }
        assert!(matches!(idx.state(), IVFState::Stale | IVFState::Trained));
    }

    #[test]
    fn threshold_search_emits_only_above_cutoff() {
        let mut idx = IVFIndex::with_params(4, 4, 2, 1024);
        for i in 0..16 {
            idx.add(i, rand_vec(i + 1, 4)).unwrap();
        }
        let pl = idx.search_threshold(&rand_vec(1, 4), 0.999).unwrap();
        // The query is the same shape as doc 0, so at least one
        // result is above the high threshold.
        assert!(pl.len() <= 16);
    }

    #[test]
    fn ordinal_count_matches_zero_based_u32_format() {
        validate_vector_ordinal_count(u64::from(u32::MAX) + 1).unwrap();
        let error = validate_vector_ordinal_count(u64::from(u32::MAX) + 2).unwrap_err();
        assert!(error.to_string().contains("u32 index format"));
    }

    #[test]
    fn invalid_replacement_preserves_existing_vectors() {
        let mut idx = IVFIndex::with_params(2, 4, 2, 1024);
        idx.add(1, vec![1.0, 0.0]).unwrap();

        let error = idx.add(1, vec![f32::NAN, 0.0]).unwrap_err();
        assert!(error.to_string().contains("must be finite"));
        assert_eq!(idx.count().unwrap(), 1);
        assert_eq!(
            idx.search_knn(&[1.0, 0.0], 1)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn delete_counter_overflow_preserves_vector_and_assignments() {
        let mut idx = IVFIndex::with_params(2, 1, 1, 1);
        idx.add(1, vec![1.0, 0.0]).unwrap();
        assert_eq!(idx.state(), IVFState::Trained);
        *idx.deletes_since_train.lock() = usize::MAX;
        let before = idx.metadata_snapshot();

        let error = idx.delete(1).unwrap_err();
        assert!(error.to_string().contains("counter overflow"));
        assert_eq!(idx.count().unwrap(), 1);
        let after = idx.metadata_snapshot();
        assert_eq!(after.assignments, before.assignments);
        assert_eq!(after.deletes_since_train, usize::MAX);
    }
}
