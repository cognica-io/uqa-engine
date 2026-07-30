//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_core::{PostingList, Predicate};
use uqa_operators::{
    AggregateOperator, ComplementOperator, CountMonoid, ExecutionContext, FacetOperator,
    FilterOperator, KNNOperator, Operator, PathFilterOperator, PathProjectOperator,
    SpatialWithinOperator, TermOperator, VectorSimilarityOperator,
};
use uqa_storage::StorageBackendResult;

struct EmptySource;

impl Operator for EmptySource {
    fn execute(&self, _ctx: &ExecutionContext) -> StorageBackendResult<PostingList> {
        Ok(PostingList::new())
    }
}

fn assert_missing_backend(result: StorageBackendResult<PostingList>, backend: &str) {
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains(backend),
        "expected {backend} backend error, got {message}"
    );
}

#[test]
fn storage_dependent_operators_do_not_turn_missing_backends_into_empty_results() {
    let context = ExecutionContext::new();
    let source: Arc<dyn Operator> = Arc::new(EmptySource);

    assert_missing_backend(
        TermOperator::new("term", "body").execute(&context),
        "inverted-index",
    );
    assert_missing_backend(
        VectorSimilarityOperator::new(vec![1.0], 0.0, "embedding").execute(&context),
        "vector-index",
    );
    assert_missing_backend(
        KNNOperator::new(vec![1.0], 1, "embedding").execute(&context),
        "vector-index",
    );
    assert_missing_backend(
        SpatialWithinOperator::new("point", 0.0, 0.0, 1.0).execute(&context),
        "document-store",
    );
    assert_missing_backend(
        FilterOperator::new("field", Predicate::IsNotNull, None).execute(&context),
        "document-store",
    );
    assert_missing_backend(
        FacetOperator::new("field", None).execute(&context),
        "document-store",
    );
    assert_missing_backend(
        ComplementOperator::new(Arc::clone(&source)).execute(&context),
        "document-store",
    );
    assert_missing_backend(
        PathFilterOperator::new(Vec::new(), Predicate::IsNull, None).execute(&context),
        "document-store",
    );
    assert_missing_backend(
        PathProjectOperator::new(Vec::new(), Arc::clone(&source)).execute(&context),
        "document-store",
    );
    assert_missing_backend(
        AggregateOperator::new(Some(source), "field", Arc::new(CountMonoid)).execute(&context),
        "document-store",
    );
}
