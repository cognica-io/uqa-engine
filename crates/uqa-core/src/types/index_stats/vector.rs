//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical vector statistics supplied by storage to query planning. These values contain no provider handle or storage algorithm.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorQueryRoute {
    Approximate,
    ExactZeroNorm,
    ExactNonFiniteNorm,
}

/// One selected immutable physical generation, independent of file names and live catalog labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorGeneration {
    pub database: [u8; 16],
    pub table: u64,
    pub index: u64,
    pub generation: u64,
}

/// Stored base populations are distinct from the current canonical population and its outstanding changes. None means unmeasured on this selected view, never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorPopulationStats {
    pub base_documents: Option<u64>,
    pub base_vectors: u64,
    pub graph_nodes: u64,
    pub side_vectors: u64,
    pub current_vectors: Option<u64>,
    pub changed_vectors: Option<u64>,
}

/// Declared resource limits and provider capabilities, not observed cache occupancy or physical I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorReadStats {
    pub resident_bytes: usize,
    pub cache_bytes: usize,
    pub max_record_bytes: usize,
    pub max_in_flight_page_bytes: usize,
    pub max_batch_pages: usize,
    pub read_concurrency: usize,
}

/// Facts about a field and query selected on the same retained physical/canonical view. Storage supplies format-derived quantities and the numeric route; Planner supplies work estimates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskANNQueryStats {
    pub generation: VectorGeneration,
    pub dimensions: u32,
    pub max_degree: usize,
    pub search_list_size: usize,
    pub beam_width: usize,
    pub pq_bytes: usize,
    pub pq_centroids: Option<u16>,
    pub node_slot_bytes: usize,
    pub node_fragments: u32,
    pub graph_pages: u64,
    pub graph_edges: Option<u64>,
    pub page_bytes: usize,
    pub populations: VectorPopulationStats,
    pub reads: VectorReadStats,
    pub query_route: VectorQueryRoute,
}
