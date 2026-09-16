//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW query selection with an exact fallback before initial graph build.

use uqa_core::PostingList;

use super::SQLiteHNSWIndex;
use uqa_storage::vector_index::VectorIndex;
use uqa_storage::StorageBackendResult;

impl SQLiteHNSWIndex {
    pub(super) fn search_top_k(
        &self,
        query: &[f32],
        k: usize,
    ) -> StorageBackendResult<PostingList> {
        if let Some(cached) = self.graph_snapshot()? {
            return cached.graph.search_knn(query, k);
        }
        if self.require_persisted_graph {
            return Err(super::mutation::missing_metadata(self));
        }
        self.persistent.search_knn(query, k)
    }
}
