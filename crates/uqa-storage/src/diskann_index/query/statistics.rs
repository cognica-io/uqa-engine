//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Successful query work is scoped to one search, independent of retained-reader reuse.

use crate::diskann_index::{pages::DiskANNPageReadStats, DiskANNScoringStats};

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
