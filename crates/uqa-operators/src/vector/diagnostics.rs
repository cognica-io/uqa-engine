//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optional observation of the same primitive that supplies an operator's postings.

use uqa_core::{vector_execution::VectorSearchOperation, PostingList};
use uqa_storage::{
    read_control::StorageReadControl, vector_index::VectorQueryResult, StorageBackendResult,
    VectorIndex,
};

/// The owner binds the relation and retains the original invocation allowance.
pub trait VectorSearchObserver: Send + Sync {
    fn control(&self) -> &StorageReadControl;
    fn record(
        &self,
        field: &str,
        operation: VectorSearchOperation,
        result: &VectorQueryResult,
    ) -> StorageBackendResult<()>;
}

pub fn search_knn(
    index: &dyn VectorIndex,
    field: &str,
    query: &[f32],
    k: usize,
    observer: Option<&dyn VectorSearchObserver>,
) -> StorageBackendResult<PostingList> {
    let Some(observer) = observer else {
        return index.search_knn(query, k);
    };
    let result = index.search_knn_with_statistics(query, k, Some(observer.control()))?;
    observer.record(field, VectorSearchOperation::KNN { k }, &result)?;
    Ok(result.postings)
}

pub fn search_threshold(
    index: &dyn VectorIndex,
    field: &str,
    query: &[f32],
    threshold: f32,
    observer: Option<&dyn VectorSearchObserver>,
) -> StorageBackendResult<PostingList> {
    let Some(observer) = observer else {
        return index.search_threshold(query, threshold);
    };
    let result =
        index.search_threshold_with_statistics(query, threshold, Some(observer.control()))?;
    observer.record(
        field,
        VectorSearchOperation::Threshold { threshold },
        &result,
    )?;
    Ok(result.postings)
}
