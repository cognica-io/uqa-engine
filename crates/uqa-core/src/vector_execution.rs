//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Data returned by one physical vector invocation; storage owns production of the counts.

/// Logical graph-page work. Provider pages are complete uncompressed pages returned by the storage interface, not measured SSD operations or bytes. Cache hits are observed while retaining each actual lease.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskANNPageReadStats {
    pub page_requests: u64,
    pub cache_hits: u64,
    pub provider_pages: u64,
    pub provider_batches: u64,
}

/// Successful vector-bearing tensor scores and their actual cosine evaluations. Repeated scoring is repeated work, not a distinct-document census.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskANNScoringStats {
    pub documents: u64,
    pub vectors: u64,
}

/// Actual page outcomes and scoring by candidate source. Graph and side entries share complete-tensor reranking; current uncovered changes and independent raw tensors retain their own attribution. Exact routes use only `exact`. These are logical counts at declared storage boundaries, not physical I/O measurements.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskANNQueryWork {
    pub pages: DiskANNPageReadStats,
    pub side_entries: u64,
    pub reranked: DiskANNScoringStats,
    pub changed: DiskANNScoringStats,
    pub unversioned: DiskANNScoringStats,
    pub exact: DiskANNScoringStats,
}

/// Counts of successful logical work, independent of cache hits or provider completion order. These are not physical I/O counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskANNTraversalStats {
    pub approximate_expansions: u64,
    pub completion_expansions: u64,
    pub pq_estimates: u64,
    pub beams: u64,
}

/// The route actually executed. Zero-k queries validate their arguments but perform no search; thresholds always use exact tensor scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskANNExecutionRoute {
    Approximate,
    ExactZeroNorm,
    ExactNonFiniteNorm,
    ExactThreshold,
    EmptyK,
}

/// The actual primitive requested by the caller, before ranking or calibration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VectorSearchOperation {
    KNN { k: usize },
    Threshold { threshold: f32 },
}
