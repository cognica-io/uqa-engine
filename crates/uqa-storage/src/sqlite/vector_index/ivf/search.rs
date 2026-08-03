//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only IVF query path.

use uqa_core::PostingList;

use super::math::{nearest_centroids, scored_posting_list};
use super::SQLiteIVFIndex;
use crate::ivf_index::IVFState;
use crate::vector_index::VectorIndex;
use crate::StorageBackendResult;

impl SQLiteIVFIndex {
    pub(super) fn search_top_k(
        &self,
        query: &[f32],
        k: usize,
    ) -> StorageBackendResult<PostingList> {
        self.persistent.validate_dimensions(query)?;
        if k == 0 {
            return Ok(PostingList::new());
        }
        let Some(meta) = self.ready_meta()? else {
            return self.persistent.search_knn(query, k);
        };
        if meta.state != IVFState::Trained {
            return self.persistent.search_knn(query, k);
        }
        let centroids = self.load_centroids()?;
        if centroids.is_empty() {
            return self.persistent.search_knn(query, k);
        }
        let probes = nearest_centroids(query, &centroids, self.params.nprobe);
        let candidates = self.load_candidates(&probes)?;
        if candidates.is_empty() {
            return Ok(PostingList::new());
        }
        Ok(scored_posting_list(query, &candidates, k))
    }
}
