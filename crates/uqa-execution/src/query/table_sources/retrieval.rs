//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::ScoredEntry;
use uqa_sql::{SQLError, SQLParam, ScalarExpr};
pub enum DirectVectorRetrieval {
    Knn {
        top_k: usize,
    },
    Calibrated {
        field: String,
        query_vector: Vec<f32>,
        top_k: usize,
        threshold: Option<f64>,
    },
}
/// Retrieval access is selected by the statement snapshot; committed reads are requested only for tuple rechecks.
pub trait RetrievalAccess: Sync {
    fn direct_vector_retrieval(
        &self,
        predicate: &ScalarExpr,
        params: &[SQLParam],
    ) -> Result<Option<DirectVectorRetrieval>, SQLError>;
    fn knn_entries(
        &self,
        table: &str,
        field: &str,
        query: &[f32],
        top_k: usize,
        committed: bool,
    ) -> Result<Vec<ScoredEntry>, SQLError>;
    fn retrieval_entries(
        &self,
        table: &str,
        predicate: &ScalarExpr,
        params: &[SQLParam],
        committed: bool,
    ) -> Result<Option<Vec<ScoredEntry>>, SQLError>;
}
