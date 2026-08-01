//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public text search and calibration reporting.

use super::{
    storage_sql_error, Arc, BayesianBM25Scorer, CalibrationMetrics, CalibrationReport, DocId,
    Engine, Instant, OperatorTree, RawBm25Score, SQLError, ScoredEntry, ScoringMode,
    TextScoringMode, TextSearchProfile,
};

impl Engine {
    /// Run a text query through the shared operator optimizer and executor.
    pub fn search(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let scoring = match mode {
            ScoringMode::BM25(params) => uqa_operators::TextScoringMode::CustomBM25(*params),
            ScoringMode::BayesianBM25(params) => {
                uqa_operators::TextScoringMode::CustomBayesianBM25(*params)
            }
        };
        let tree = self.plan_text_top_k_tree(table, field, query, mode, scoring, top_k)?;
        let entries = crate::operator_tree_bridge::execute_scored_tree(self, table, &[], &tree)?;
        Ok(Self::rank_scored_entries_top_k(entries, top_k))
    }

    /// Run the same planner-selected physical text path as [`Self::search`]
    /// while returning exact candidate/skip counters for benchmarks and
    /// production diagnostics.
    pub fn search_profiled(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
    ) -> Result<TextSearchProfile, SQLError> {
        let started = Instant::now();
        let scoring = match mode {
            ScoringMode::BM25(params) => TextScoringMode::CustomBM25(*params),
            ScoringMode::BayesianBM25(params) => TextScoringMode::CustomBayesianBM25(*params),
        };
        let tree = self.plan_text_top_k_tree(table, field, query, mode, scoring, top_k)?;
        let physical_top_k = match tree {
            OperatorTree::Term { top_k, .. } => top_k,
            _ => None,
        };
        let mut profile =
            self.search_leaf_profiled(table, field, query, mode, top_k, physical_top_k)?;
        profile.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(profile)
    }

    /// Compute calibration diagnostics for a Bayesian BM25 query
    /// against every document in `table`, aligned to `labels` in
    /// ascending document-id order.
    pub fn calibration_report(
        &self,
        table: &str,
        field: &str,
        query: &str,
        labels: &[u8],
    ) -> Result<CalibrationReport, SQLError> {
        let Some(table_state) = self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve calibration table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let doc_ids = self.table_doc_ids(table)?;
        if labels.len() != doc_ids.len() {
            return Err(SQLError::TypeMismatch(format!(
                "labels length ({}) must match document count ({})",
                labels.len(),
                doc_ids.len()
            )));
        }
        if labels.iter().any(|label| *label > 1) {
            return Err(SQLError::TypeMismatch(
                "labels must contain only 0 or 1".into(),
            ));
        }

        let params = self.bayesian_params_for(table, field)?;
        let (query_term_count, stats) = {
            let index = table_state.inverted_index.read();
            let query_term_count = index
                .get_search_analyzer(field)
                .analyze(query)
                .map_err(|error| storage_sql_error("analyze calibration query", error))?
                .len();
            let stats = Arc::new(
                index
                    .field_stats(field)
                    .map_err(|error| storage_sql_error("read calibration field stats", error))?,
            );
            (query_term_count, stats)
        };
        let scaled_params = params.scaled_for_query_terms(query_term_count);
        let non_match_probability = BayesianBM25Scorer::new(scaled_params, stats)
            .map_err(|error| {
                SQLError::Internal(format!("build calibration-report scorer: {error}"))
            })?
            .calibrate_raw_score(
                RawBm25Score::new(0.0).expect("the raw BM25 score for a non-match is finite"),
            )
            .value();

        let mode = ScoringMode::BayesianBM25(params);
        let score_map: std::collections::BTreeMap<DocId, f64> = self
            .search(table, field, query, &mode, usize::MAX)?
            .into_iter()
            .map(|entry| (entry.doc_id, entry.score))
            .collect();
        let probabilities: Vec<f64> = doc_ids
            .iter()
            .map(|doc_id| {
                score_map
                    .get(doc_id)
                    .copied()
                    .unwrap_or(non_match_probability)
            })
            .collect();
        CalibrationMetrics::report(&probabilities, labels, 10)
            .map_err(|error| SQLError::Internal(format!("compute calibration report: {error}")))
    }
}
