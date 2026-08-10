//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF configuration and mutable state.

use std::collections::BTreeMap;

use parking_lot::Mutex;
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

#[derive(Debug, Clone)]
pub(crate) struct IVFMetadataSnapshot {
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

pub struct IVFIndex {
    pub(super) dimensions: u32,
    pub(super) nlist: usize,
    pub(super) nprobe: Mutex<usize>,
    pub(super) train_threshold: usize,
    pub(super) state: Mutex<IVFState>,
    pub(super) vectors: Mutex<BTreeMap<VectorKey, StoredVector>>,
    pub(super) centroids: Mutex<Vec<Vec<f32>>>,
    pub(super) inverted_lists: Mutex<Vec<Vec<VectorKey>>>,
    pub(super) trained_size: Mutex<usize>,
    pub(super) deletes_since_train: Mutex<usize>,
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

    pub(crate) fn metadata_snapshot(&self) -> IVFMetadataSnapshot {
        let vectors = self.vectors.lock();
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
            state: *self.state.lock(),
            centroids: self.centroids.lock().clone(),
            assignments,
            trained_size: *self.trained_size.lock(),
            deletes_since_train: *self.deletes_since_train.lock(),
            vector_count: vectors.len(),
        }
    }

    pub(crate) fn detached_clone(&self) -> Self {
        Self {
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
        }
    }
}
