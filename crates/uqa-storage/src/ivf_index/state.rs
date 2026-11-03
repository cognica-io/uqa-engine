//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF configuration and mutable state.

use std::collections::BTreeMap;

use uqa_core::DocId;

const DEFAULT_NLIST: usize = 100;
const DEFAULT_NPROBE: usize = 10;
const DEFAULT_TRAIN_THRESHOLD: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IVFState {
    Untrained,
    Trained,
    Stale,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IVFMetadataSnapshot {
    pub state: IVFState,
    pub centroids: Vec<Vec<f32>>,
    pub assignments: Vec<(DocId, u32, usize)>,
    pub trained_size: usize,
    pub deletes_since_train: usize,
    pub vector_count: usize,
}

pub(super) type VectorKey = (DocId, u32);

#[derive(Debug, Clone)]
pub(super) struct StoredVector {
    pub(super) key: VectorKey,
    pub(super) doc_id: DocId,
    pub(super) vector_ordinal: u32,
    pub(super) raw_vector: Vec<f32>,
    pub(super) norm: f32,
    pub(super) vector: Vec<f32>,
    pub(super) centroid: Option<usize>,
}

pub(super) struct IVFData {
    pub(super) dimensions: u32,
    pub(super) nlist: usize,
    pub(super) nprobe: usize,
    pub(super) train_threshold: usize,
    pub(super) state: IVFState,
    pub(super) vectors: BTreeMap<VectorKey, StoredVector>,
    pub(super) centroids: Vec<Vec<f32>>,
    pub(super) inverted_lists: Vec<Vec<VectorKey>>,
    pub(super) trained_size: usize,
    pub(super) deletes_since_train: usize,
}

impl IVFData {
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
            nprobe: nprobe.max(1),
            train_threshold: train_threshold.max(1),
            state: IVFState::Untrained,
            vectors: BTreeMap::new(),
            centroids: Vec::new(),
            inverted_lists: Vec::new(),
            trained_size: 0,
            deletes_since_train: 0,
        }
    }

    pub fn state(&self) -> IVFState {
        self.state
    }

    pub fn nprobe(&self) -> usize {
        self.nprobe
    }

    pub fn set_nprobe(&mut self, nprobe: usize) {
        self.nprobe = nprobe.max(1);
    }

    pub fn metadata_snapshot(&self) -> IVFMetadataSnapshot {
        let vectors = &self.vectors;
        let mut assignments = vectors
            .values()
            .filter_map(|vector| {
                vector
                    .centroid
                    .map(|centroid| (vector.doc_id, vector.vector_ordinal, centroid))
            })
            .collect::<Vec<_>>();
        assignments.sort_by_key(|(doc_id, ordinal, _)| (*doc_id, *ordinal));
        IVFMetadataSnapshot {
            state: self.state,
            centroids: self.centroids.clone(),
            assignments,
            trained_size: self.trained_size,
            deletes_since_train: self.deletes_since_train,
            vector_count: vectors.len(),
        }
    }
}
