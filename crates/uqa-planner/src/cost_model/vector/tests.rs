//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::cost_model::CostModel;
use uqa_core::{
    DiskANNIndexStats, IndexStats, VectorGeneration, VectorPopulationStats, VectorReadStats,
};
use uqa_operators::OperatorTree;

fn facts() -> DiskANNQueryStats {
    DiskANNQueryStats {
        index: DiskANNIndexStats {
            generation: VectorGeneration {
                database: [1; 16],
                table: 1,
                index: 2,
                generation: 3,
            },
            dimensions: 1024,
            max_degree: 128,
            search_list_size: 12,
            beam_width: 4,
            pq_bytes: 2,
            pq_centroids: Some(3),
            node_slot_bytes: 5184,
            node_fragments: 2,
            graph_pages: 200,
            graph_edges: Some(400),
            page_bytes: 4096,
            populations: VectorPopulationStats {
                base_documents: Some(60),
                base_vectors: 120,
                graph_nodes: 100,
                side_vectors: 20,
                current_vectors: Some(200),
                changed_vectors: Some(6),
            },
            reads: VectorReadStats {
                resident_bytes: 1 << 20,
                cache_bytes: 0,
                max_record_bytes: 1 << 20,
                max_in_flight_page_bytes: 3 * 4096,
                max_batch_pages: 3,
                read_concurrency: 2,
            },
        },
        query_route: VectorQueryRoute::Approximate,
    }
}

#[test]
fn diskann_cost_separates_pq_pages_dispatch_and_complete_tensor_work() {
    let work = estimate_diskann(&facts(), Some(3), 100);
    assert_eq!((work.approximate_nodes, work.completion_nodes), (12.0, 0.0));
    assert_eq!(
        (work.pq_lookup_coordinates, work.pq_distance_evaluations),
        (3072.0, 49.0)
    );
    assert_eq!(
        (work.logical_page_requests, work.logical_page_bytes),
        (24.0, 98304.0)
    );
    // Three beams each read eight pages in 3/3/2 batches at concurrency two: 2+2+1 rounds.
    assert_eq!(work.page_rounds, 15.0);
    assert_eq!(
        (work.side_vectors, work.changed_vectors, work.rerank_vectors),
        (20.0, 6.0, 24.0)
    );
    assert_eq!(work.cpu, 74850.0);
    assert_eq!(work.io, 312.0);
    assert_eq!(work.total(), 75162.0);
    assert_eq!(work.resident_pq_payload_bytes, 24776.0);
    assert!(work.page_budget_sufficient);
}

#[test]
fn diskann_cost_keeps_logical_beams_separate_from_provider_limits() {
    let mut physical = facts();
    let original = estimate_diskann(&physical, Some(10), 100);
    assert_eq!(
        (original.approximate_nodes, original.completion_nodes),
        (12.0, 8.0)
    );
    assert_eq!(original.page_rounds, 25.0);
    physical.index.reads.max_in_flight_page_bytes = 4096;
    let serial = estimate_diskann(&physical, Some(10), 100);
    assert_eq!(serial.logical_page_requests, 40.0);
    assert_eq!(serial.page_rounds, 40.0);
    assert_eq!(serial.cpu, original.cpu);
    assert_eq!(
        serial.pq_distance_evaluations,
        original.pq_distance_evaluations
    );
    physical.index.reads.max_in_flight_page_bytes = 0;
    assert!(!estimate_diskann(&physical, Some(10), 100).page_budget_sufficient);
}

#[test]
fn diskann_exact_routes_and_zero_k_do_not_inherit_ann_work() {
    let mut physical = facts();
    let threshold = estimate_diskann(&physical, None, 100);
    assert_eq!(threshold.exact_vectors, 200.0);
    assert_eq!(threshold.total(), 204_800.0);
    assert_eq!(
        (
            threshold.logical_page_requests,
            threshold.pq_distance_evaluations
        ),
        (0.0, 0.0)
    );
    for route in [
        VectorQueryRoute::ExactZeroNorm,
        VectorQueryRoute::ExactNonFiniteNorm,
    ] {
        physical.query_route = route;
        assert_eq!(estimate_diskann(&physical, Some(3), 100), threshold);
    }
    assert_eq!(estimate_diskann(&physical, Some(0), 100).total(), 0.0);
    physical.index.populations.current_vectors = None;
    physical.index.populations.changed_vectors = None;
    physical.query_route = VectorQueryRoute::Approximate;
    let unknown = estimate_diskann(&physical, Some(3), 100);
    assert_eq!(unknown.changed_vectors, 200.0);
    assert_eq!(physical.index.populations.changed_vectors, None);
}

#[test]
fn diskann_cost_uses_each_field_and_exact_query_identity() {
    let mut stats = IndexStats::new(100);
    stats.dimensions = 9999;
    let physical = facts();
    let mut query = vec![0.0; 1024];
    query[0] = 1.0;
    let zero = vec![0.0; 1024];
    stats.set_diskann_query("embedding", &query, physical.clone());
    let mut exact = physical.clone();
    exact.query_route = VectorQueryRoute::ExactZeroNorm;
    stats.set_diskann_query("embedding", &zero, exact);
    let mut other = physical.clone();
    other.index.dimensions = 4;
    other.index.node_slot_bytes = 1104;
    other.index.node_fragments = 1;
    other.index.graph_pages = 34;
    stats.set_diskann_query("other", &[1.0, 0.0, 0.0, 0.0], other);
    let model = CostModel::new();
    for (field, query, expected) in [
        ("embedding", query.clone(), 75162.0),
        ("embedding", zero.clone(), 204_800.0),
        ("other", vec![1.0, 0.0, 0.0, 0.0], 558.0),
    ] {
        assert_eq!(
            model.estimate(
                &OperatorTree::KNN {
                    field: field.into(),
                    query_vector: query,
                    k: 3
                },
                &stats
            ),
            expected
        );
    }
    assert!(stats.diskann_query("missing", &query).is_none());
    let mut negative_zero = zero;
    negative_zero[0] = -0.0;
    assert!(stats.diskann_query("embedding", &negative_zero).is_none());
    assert_eq!(
        model.estimate(
            &OperatorTree::VectorSimilarity {
                field: "embedding".into(),
                query_vector: query,
                threshold: 0.5
            },
            &stats
        ),
        204_800.0
    );
}
