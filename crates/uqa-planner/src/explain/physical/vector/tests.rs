//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;

struct FailingMetadata(Cell<usize>);
impl VectorStatisticsRead for FailingMetadata {
    fn dimensions(&self, field: &str) -> Option<u32> {
        (field == "embedding").then_some(2)
    }
    fn diskann_index_statistics(&self, _: &str) -> Result<Option<DiskANNIndexStats>, SQLError> {
        self.0.set(self.0.get() + 1);
        Err(SQLError::Internal(
            "selected metadata is unavailable".into(),
        ))
    }
    fn diskann_query_statistics(
        &self,
        _: &str,
        _: &[f32],
    ) -> Result<Option<uqa_core::DiskANNQueryStats>, SQLError> {
        self.0.set(self.0.get() + 1);
        Err(SQLError::Internal(
            "selected metadata is unavailable".into(),
        ))
    }
}

#[test]
fn diskann_explain_invalid_inputs_do_not_preempt_execution_with_metadata_failures() {
    let reader = FailingMetadata(Cell::new(0));
    let documents = || panic!("a failed or deferred field has no concrete query-work estimate");
    for (field, query) in [
        ("missing", vec![1.0, 0.0]),
        ("embedding", vec![]),
        ("embedding", vec![1.0]),
        ("embedding", vec![f32::NAN, 0.0]),
    ] {
        assert!(
            physical_vector(&reader, field, Some((&query, 1)), &documents)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(reader.0.get(), 0);
    assert!(physical_vector(&reader, "embedding", None, &documents).is_err());
    assert_eq!(reader.0.get(), 1);
    assert!(physical_vector(
        &reader,
        "embedding",
        Some((&[f32::MAX, 0.0], 1)),
        &documents
    )
    .is_err());
    assert_eq!(reader.0.get(), 2);
}
