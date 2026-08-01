//! Hybrid text/vector query construction and execution.

use super::{storage_sql_error, Engine, HybridSearchParams, SQLError, ScoredEntry};

impl Engine {
    /// Robust hybrid retrieval: query-level Bayesian BM25 over `text_field`
    /// and query-pool-transformed KNN over `vector_field`, combined by
    /// positive-evidence pooling.
    /// Both signals enter as prior-free evidence -- BM25 by one sigmoid
    /// over the complete raw query score with the prior stripped, and
    /// cosine distance by the pool-fitted two-Gaussian calibration --
    /// while the text field's estimated relevance prior enters the
    /// pool exactly once. Signals are weighted per query by their
    /// gated-evidence spread over the candidate pool, so a signal that
    /// cannot separate the candidates loses influence instead of
    /// diluting the fused ranking. Returns the top-`top_k` entries by
    /// descending pooled score. This API intentionally exposes a robust ranking
    /// heuristic, not the exact conditional-independence contract provided by
    /// `fuse_bayesian_evidence` / `BayesianEvidenceFusion`.
    pub fn hybrid_search(&self, params: &HybridSearchParams) -> Result<Vec<ScoredEntry>, SQLError> {
        let Some(table) = self
            .try_table(params.table)
            .map_err(|error| storage_sql_error("resolve hybrid-search table", error))?
        else {
            return Err(SQLError::UnknownTable(params.table.to_string()));
        };
        self.validate_text_search_field(params.table, params.text_field)?;
        let analyzer = table
            .inverted_index
            .read()
            .get_search_analyzer(params.text_field);
        let analyzed_terms = analyzer
            .analyze(params.text_query)
            .map_err(|error| storage_sql_error("analyze hybrid text query", error))?;
        let mut signals = Vec::new();
        if !analyzed_terms.is_empty() {
            signals.push(uqa_operators::OperatorTree::Term {
                query: params.text_query.to_string(),
                field: Some(params.text_field.to_string()),
                scoring: Some(uqa_operators::TextScoringMode::BayesianBM25),
                top_k: None,
            });
        }

        // The vector field is part of the hybrid API contract even when its
        // candidate pool is empty. Always lower the leaf so EngineDriver
        // validates field existence, index availability, dimensions, and
        // finite query values instead of silently degrading to text-only.
        signals.push(uqa_operators::OperatorTree::CalibratedVectorMatch {
            query_vector: params.query_vector.clone(),
            k: params.knn_pool,
            field: params.vector_field.to_string(),
            threshold: None,
        });
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
}
