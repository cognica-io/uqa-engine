//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::vector_statistics;
use crate::retrieval_planning::VectorStatisticsRead;
use std::cell::Cell;
use uqa_core::{memory::ProductionControl, DiskANNQueryStats, IndexStatsBuilder};
use uqa_sql::SQLError;

mod collection;

struct Vectors(Cell<usize>);

impl VectorStatisticsRead for Vectors {
    fn dimensions(&self, field: &str) -> Option<u32> {
        (field == "embedding").then_some(2)
    }

    fn diskann_query_statistics(
        &self,
        field: &str,
        query: &[f32],
    ) -> Result<Option<DiskANNQueryStats>, SQLError> {
        assert_eq!(field, "embedding");
        assert_eq!(query, [f32::MAX, 0.0]);
        self.0.set(self.0.get() + 1);
        Ok(None)
    }
}

#[test]
fn physical_vector_cost_inputs_preserve_execution_validation() {
    let vectors = Vectors(Cell::new(0));
    let control = ProductionControl::uncontrolled();
    let mut stats = IndexStatsBuilder::new(1, control);
    for query in [
        vec![],
        vec![1.0],
        vec![1.0, 0.0, 0.0],
        vec![f32::NAN, 0.0],
        vec![f32::INFINITY, 0.0],
        vec![f32::NEG_INFINITY, 0.0],
    ] {
        vector_statistics(&vectors, &mut stats, "embedding", &query, &control).unwrap();
    }
    vector_statistics(&vectors, &mut stats, "missing", &[1.0, 0.0], &control).unwrap();
    assert_eq!(vectors.0.get(), 0);
    assert_eq!(stats.dimensions, 3);
    // Finite coordinates with an overflowing norm must still reach Storage's numeric-route classification.
    vector_statistics(
        &vectors,
        &mut stats,
        "embedding",
        &[f32::MAX, 0.0],
        &control,
    )
    .unwrap();
    assert_eq!(vectors.0.get(), 1);
}
