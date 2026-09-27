//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Results and diagnostics share one successful physical invocation.

use crate::diskann_index::{
    format::DiskANNGeneration, search::DiskANNTraversalStats, DiskANNQueryWork,
};
use uqa_core::PostingList;

pub use uqa_core::vector_execution::DiskANNExecutionRoute;

/// Bounded observations from the same retained physical/canonical view that produced the result. Page counts describe logical storage work, not device I/O; no metadata probe or second query is needed to produce this report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskANNExecutionStats {
    pub generation: DiskANNGeneration,
    pub route: DiskANNExecutionRoute,
    pub traversal: DiskANNTraversalStats,
    pub work: DiskANNQueryWork,
}

/// One result with optional measured work. An unsupported index returns `None`, never fabricated zero work. Failed invocations return an error without a successful report.
pub struct VectorQueryResult {
    pub postings: PostingList,
    pub diskann: Option<DiskANNExecutionStats>,
}

impl VectorQueryResult {
    pub(crate) fn unmeasured(postings: PostingList) -> Self {
        Self {
            postings,
            diskann: None,
        }
    }

    pub(crate) fn measured(
        result: crate::diskann_index::DiskANNQueryResult,
        generation: DiskANNGeneration,
        route: DiskANNExecutionRoute,
    ) -> Self {
        Self {
            postings: result.postings,
            diskann: Some(DiskANNExecutionStats {
                generation,
                route,
                traversal: result.traversal,
                work: result.work,
            }),
        }
    }
}

#[cfg(test)]
mod tests;
