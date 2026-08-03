//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW query selection with an exact fallback before initial graph build.

use uqa_core::PostingList;

use super::SQLiteHNSWIndex;
use crate::vector_index::VectorIndex;
use crate::StorageBackendResult;

impl SQLiteHNSWIndex {
    pub(super) fn search_top_k(
        &self,
        query: &[f32],
        k: usize,
    ) -> StorageBackendResult<PostingList> {
        if self.persisted_revision()?.is_some() {
            return self.cached_graph()?.search_knn(query, k);
        }
        if self.require_persisted_graph {
            return Err(super::mutation::missing_metadata(self));
        }
        self.persistent.search_knn(query, k)
    }
}
