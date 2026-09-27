//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained physical invocation records, separate from estimated plan nodes.

use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, BudgetedString, BudgetedVec},
    vector_execution::{
        DiskANNExecutionRoute, DiskANNQueryWork, DiskANNTraversalStats, VectorSearchOperation,
    },
    VectorGeneration,
};

pub type ExplainVectorSearches = BudgetedVec<Arc<Budgeted<ExplainVectorSearch>>>;

/// One successful `DiskANN` primitive. Names and record storage retain their original allowance.
#[derive(Debug)]
pub struct ExplainVectorSearch {
    pub relation: BudgetedString,
    pub field: BudgetedString,
    pub operation: VectorSearchOperation,
    pub returned_documents: u64,
    pub generation: VectorGeneration,
    pub route: DiskANNExecutionRoute,
    pub traversal: DiskANNTraversalStats,
    pub work: DiskANNQueryWork,
}
