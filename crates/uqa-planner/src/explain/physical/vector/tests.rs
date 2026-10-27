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
            physical_vector(&reader, field, Some((&query, 1)), true, &documents)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(reader.0.get(), 0);
    assert!(physical_vector(&reader, "embedding", None, true, &documents).is_err());
    assert_eq!(reader.0.get(), 1);
    assert!(physical_vector(
        &reader,
        "embedding",
        Some((&[f32::MAX, 0.0], 1)),
        true,
        &documents
    )
    .is_err());
    assert_eq!(reader.0.get(), 2);
}

struct SelectedMetadata;

impl VectorStatisticsRead for SelectedMetadata {
    fn dimensions(&self, _: &str) -> Option<u32> {
        Some(2)
    }

    fn diskann_query_statistics(
        &self,
        _: &str,
        _: &[f32],
    ) -> Result<Option<uqa_core::DiskANNQueryStats>, SQLError> {
        Ok(Some(uqa_core::DiskANNQueryStats {
            query_route: VectorQueryRoute::Approximate,
            index: DiskANNIndexStats {
                generation: uqa_core::VectorGeneration {
                    database: [1; 16],
                    table: 1,
                    index: 2,
                    generation: 3,
                },
                dimensions: 2,
                max_degree: 2,
                search_list_size: 4,
                beam_width: 1,
                pq_bytes: 1,
                pq_centroids: Some(2),
                node_slot_bytes: 64,
                node_fragments: 1,
                graph_pages: 1,
                graph_edges: Some(2),
                page_bytes: 4096,
                populations: uqa_core::VectorPopulationStats {
                    base_documents: Some(2),
                    base_vectors: 2,
                    graph_nodes: 2,
                    side_vectors: 0,
                    current_vectors: Some(2),
                    changed_vectors: Some(0),
                },
                reads: uqa_core::VectorReadStats {
                    resident_bytes: 4096,
                    cache_bytes: 4096,
                    max_record_bytes: 4096,
                    max_in_flight_page_bytes: 4096,
                    max_batch_pages: 1,
                    read_concurrency: 1,
                },
            },
        }))
    }
}

#[test]
fn costs_false_preserves_physical_route_without_reading_cost_populations() {
    let physical = physical_vector(
        &SelectedMetadata,
        "embedding",
        Some((&[1.0, 0.0], 1)),
        false,
        &|| panic!("COSTS FALSE must not request a cost population"),
    )
    .unwrap()
    .unwrap();
    assert!(physical.estimate.is_none());
    assert_eq!(physical.route, Some(VectorQueryRoute::Approximate));
    let reads = Cell::new(0);
    let estimated = physical_vector(
        &SelectedMetadata,
        "embedding",
        Some((&[1.0, 0.0], 1)),
        true,
        &|| {
            reads.set(reads.get() + 1);
            Ok(2)
        },
    )
    .unwrap()
    .unwrap();
    assert!(estimated.estimate.is_some());
    assert_eq!(reads.get(), 1);
}
