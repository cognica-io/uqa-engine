//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hybrid text/vector query construction and execution.

use super::{
    storage_sql_error, Engine, HybridSearchParams, RobustHybridSearchParams, SQLError, ScoredEntry,
};

impl Engine {
    /// Exact single-prior hybrid retrieval under cross-modal conditional
    /// independence. Query-level Bayesian BM25 and query-pool-transformed KNN
    /// emit signed prior-free evidence, the evidence logits add without
    /// gating or confidence scaling, and the resolved corpus relevance prior
    /// enters exactly once. Returns the top-`top_k` entries by descending
    /// posterior score.
    pub fn hybrid_search(&self, params: &HybridSearchParams) -> Result<Vec<ScoredEntry>, SQLError> {
        let signals = self.build_hybrid_signals(
            params.table,
            params.text_field,
            params.text_query,
            params.vector_field,
            &params.query_vector,
            params.knn_pool,
        )?;
        let tree = uqa_operators::OperatorTree::BayesianEvidenceFusion {
            signals,
            base_rate: None,
        };
        let entries =
            crate::operator_tree_bridge::execute_scored_tree(self, params.table, &[], &tree)?;
        Ok(Self::rank_scored_entries_top_k(entries, params.top_k))
    }

    /// Explicit robust positive-evidence hybrid ranking. This method applies
    /// Softplus gating, confidence scaling, and adaptive query-pool weights;
    /// its output is a bounded ranking heuristic rather than the exact
    /// single-prior posterior returned by [`Self::hybrid_search`].
    pub fn robust_hybrid_search(
        &self,
        params: &RobustHybridSearchParams,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let signals = self.build_hybrid_signals(
            params.table,
            params.text_field,
            params.text_query,
            params.vector_field,
            &params.query_vector,
            params.knn_pool,
        )?;
        let tree = uqa_operators::OperatorTree::RobustPositiveEvidencePool {
            signals,
            alpha: params.alpha,
            gating: uqa_operators::GatingSpec::Softplus,
            weights: None,
            logit_min: None,
            logit_max: None,
            adaptive_weights: true,
        };
        let entries =
            crate::operator_tree_bridge::execute_scored_tree(self, params.table, &[], &tree)?;
        Ok(Self::rank_scored_entries_top_k(entries, params.top_k))
    }

    fn build_hybrid_signals(
        &self,
        table_name: &str,
        text_field: &str,
        text_query: &str,
        vector_field: &str,
        query_vector: &[f32],
        knn_pool: usize,
    ) -> Result<Vec<uqa_operators::OperatorTree>, SQLError> {
        let Some(table) = self
            .try_table(table_name)
            .map_err(|error| storage_sql_error("resolve hybrid-search table", error))?
        else {
            return Err(SQLError::UnknownTable(table_name.to_string()));
        };
        self.validate_text_search_field(table_name, text_field)?;
        let analyzer = table.inverted_index.read().get_search_analyzer(text_field);
        let analyzed_terms = analyzer
            .analyze(text_query)
            .map_err(|error| storage_sql_error("analyze hybrid text query", error))?;
        let mut signals = Vec::new();
        if !analyzed_terms.is_empty() {
            signals.push(uqa_operators::OperatorTree::Term {
                query: text_query.to_string(),
                field: Some(text_field.to_string()),
                scoring: Some(uqa_operators::TextScoringMode::BayesianBM25),
                top_k: None,
            });
        }

        // The vector field is part of the hybrid API contract even when its
        // candidate pool is empty. Always lower the leaf so EngineDriver
        // validates field existence, index availability, dimensions, and
        // finite query values instead of silently degrading to text-only.
        signals.push(uqa_operators::OperatorTree::CalibratedVectorMatch {
            query_vector: query_vector.to_vec(),
            k: knn_pool,
            field: vector_field.to_string(),
            threshold: None,
        });
        Ok(signals)
    }
}
