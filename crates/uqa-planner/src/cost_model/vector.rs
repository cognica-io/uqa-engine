//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DiskANN work estimates from retained physical facts. These are uncalibrated work units, not elapsed time or measured SSD operations.

use uqa_core::{DiskANNQueryStats, VectorQueryRoute};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DiskANNWorkEstimate {
    pub approximate_nodes: f64,
    pub completion_nodes: f64,
    pub pq_lookup_coordinates: f64,
    pub pq_distance_evaluations: f64,
    pub logical_page_requests: f64,
    pub logical_page_bytes: f64,
    pub page_rounds: f64,
    pub side_vectors: f64,
    pub changed_vectors: f64,
    pub rerank_vectors: f64,
    pub exact_vectors: f64,
    pub resident_pq_payload_bytes: f64,
    pub page_budget_sufficient: bool,
    pub cpu: f64,
    pub io: f64,
}

impl DiskANNWorkEstimate {
    pub fn total(self) -> f64 {
        self.cpu + self.io
    }
}

/// None selects exact threshold retrieval; Some(k) selects a document candidate pool. Unknown current/change counts use the table population and stored tensor density as explicit estimates, never an invented measured zero.
pub fn estimate_diskann(
    stats: &DiskANNQueryStats,
    k: Option<usize>,
    table_documents: u64,
) -> DiskANNWorkEstimate {
    let mut work = DiskANNWorkEstimate::default();
    let populations = stats.populations;
    let dimensions = f64::from(stats.dimensions);
    let density = populations
        .base_documents
        .filter(|count| *count != 0)
        .map_or(1.0, |documents| {
            (populations.base_vectors as f64 / documents as f64).max(1.0)
        });
    let current = populations
        .current_vectors
        .map_or(table_documents as f64 * density, |count| count as f64);
    let centroids = stats
        .pq_centroids
        .map_or(populations.graph_nodes.min(256) as f64, f64::from);
    work.resident_pq_payload_bytes =
        dimensions * centroids * 8.0 + populations.graph_nodes as f64 * stats.pq_bytes as f64;
    work.page_budget_sufficient = true;
    if k == Some(0) {
        return work;
    }
    if k.is_none() || stats.query_route != VectorQueryRoute::Approximate {
        work.exact_vectors = current;
        work.cpu = dimensions * current;
        return work;
    }
    let nodes = populations.graph_nodes as f64;
    work.approximate_nodes = nodes.min(stats.search_list_size as f64);
    let requested_vectors = k.unwrap_or_default() as f64 * density;
    work.completion_nodes = (requested_vectors - work.approximate_nodes)
        .max(0.0)
        .min(nodes - work.approximate_nodes);
    let expanded = work.approximate_nodes + work.completion_nodes;
    work.pq_lookup_coordinates = dimensions * centroids;
    work.pq_distance_evaluations = if nodes == 0.0 {
        0.0
    } else {
        let degree = stats
            .graph_edges
            .map_or((stats.max_degree as f64).min(nodes - 1.0), |edges| {
                edges as f64 / nodes
            });
        1.0 + work.approximate_nodes * degree
    };
    work.side_vectors = populations.side_vectors as f64;
    work.changed_vectors = populations
        .changed_vectors
        .map_or(current, |count| count as f64);
    // Each selected document is scored once with its complete tensor. Stored density is a planning assumption, not a bound on a particular tensor.
    work.rerank_vectors = (expanded * density).min(current);
    work.logical_page_requests = expanded * f64::from(stats.node_fragments);
    work.logical_page_bytes = work.logical_page_requests * stats.page_bytes as f64;
    work.page_rounds = page_rounds(stats, expanded);
    work.page_budget_sufficient =
        expanded == 0.0 || stats.reads.max_in_flight_page_bytes >= stats.page_bytes;
    work.cpu = work.pq_lookup_coordinates
        + work.pq_distance_evaluations * stats.pq_bytes as f64
        + dimensions * (work.side_vectors * density + work.changed_vectors + work.rerank_vectors);
    // Transfer and latency terms use fixed relative weights; no machine calibration is implied.
    work.io = 8.0 * (work.logical_page_requests + work.page_rounds);
    work
}

fn page_rounds(stats: &DiskANNQueryStats, expanded: f64) -> f64 {
    if expanded == 0.0 {
        return 0.0;
    }
    let beam = stats.beam_width.max(1) as f64;
    let batch = stats
        .reads
        .max_batch_pages
        .min(stats.reads.max_in_flight_page_bytes / stats.page_bytes.max(1));
    // An inadmissible page budget remains visible in the estimate. Serial work describes what is required, not permission to dispatch with that allowance.
    let batch = batch.max(1) as f64;
    let concurrency = (stats.reads.read_concurrency.max(1) as f64).min(batch);
    let full_beams = (expanded / beam).floor();
    let remainder = expanded - full_beams * beam;
    let fragments = f64::from(stats.node_fragments);
    let rounds = |pages: f64| {
        let complete = (pages / batch).floor();
        complete * (batch / concurrency).ceil() + ((pages - complete * batch) / concurrency).ceil()
    };
    full_beams * rounds(beam * fragments) + rounds(remainder * fragments)
}

#[cfg(test)]
mod tests;
