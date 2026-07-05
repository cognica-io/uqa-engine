//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use super::{
    Arc, BM25Scorer, BayesianBM25Params, BayesianBM25Scorer, CalibrationMetrics, CalibrationReport,
    CosineProbabilityOperator, DocId, Engine, ExecutionContext, HybridSearchParams, InvertedIndex,
    KNNOperator, LogOddsFusionOperator, Operator, ParameterLearner, PostingList, SQLError,
    ScoreOperator, ScoredEntry, Scorer, ScoringMode, TermOperator, VectorSimilarityOperator,
};
use uqa_core::IndexStats;

fn search_stats_for_terms(
    index: &dyn InvertedIndex,
    field: &str,
    terms: &[String],
    doc_freqs: &[u64],
) -> IndexStats {
    let mut stats = IndexStats::default();
    let total_docs = index.doc_count();
    stats.total_docs = total_docs;
    if total_docs > 0 {
        let total_length: u64 = index
            .field_names()
            .iter()
            .map(|indexed_field| index.total_field_length(indexed_field))
            .sum();
        if total_length > 0 {
            stats.avg_doc_length = total_length as f64 / total_docs as f64;
        }
    }

    let mut seen = BTreeSet::<&str>::new();
    for (term, doc_freq) in terms.iter().zip(doc_freqs) {
        if seen.insert(term.as_str()) {
            stats.set_doc_freq(field.to_string(), term.clone(), *doc_freq);
        }
    }
    stats
}

impl Engine {
    pub(crate) fn snapshot_context(
        &self,
        table: &str,
    ) -> Option<(ExecutionContext, Arc<uqa_core::IndexStats>)> {
        let t = self.table(table)?;
        let inv = t.inverted_index.read().snapshot();
        let stats = inv.stats();
        let stats_arc = Arc::new(stats.clone());
        let docs = t.document_store.read().snapshot();

        let mut ctx = ExecutionContext::new()
            .with_inverted_index(inv)
            .with_document_store(docs)
            .with_stats(stats);

        for (field, idx) in t.vector_indexes.read().iter() {
            ctx = ctx.with_vector_index(field.clone(), idx.snapshot());
        }

        Some((ctx, stats_arc))
    }

    fn build_text_scorer(
        mode: &ScoringMode,
        stats_arc: Arc<uqa_core::IndexStats>,
    ) -> Arc<dyn Scorer> {
        match mode {
            ScoringMode::BM25(p) => Arc::new(BM25Scorer::new(*p, stats_arc)),
            ScoringMode::BayesianBM25(p) => Arc::new(BayesianBM25Scorer::new(*p, stats_arc)),
        }
    }

    fn rank_top_k(pl: &PostingList, top_k: usize) -> Vec<ScoredEntry> {
        let entries: Vec<ScoredEntry> = pl.iter().map(ScoredEntry::from_entry).collect();
        Self::rank_scored_entries_top_k(entries, top_k)
    }

    fn rank_scored_entries_top_k(mut entries: Vec<ScoredEntry>, top_k: usize) -> Vec<ScoredEntry> {
        if top_k == 0 {
            return Vec::new();
        }
        if top_k < entries.len() {
            entries.select_nth_unstable_by(top_k, Self::compare_scored_entry_desc);
            entries.truncate(top_k);
        }
        entries.sort_by(Self::compare_scored_entry_desc);
        entries.truncate(top_k);
        entries
    }

    fn compare_scored_entry_desc(a: &ScoredEntry, b: &ScoredEntry) -> Ordering {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.doc_id.cmp(&b.doc_id))
    }

    /// Run a single-term or multi-term `text_match` query against `field`
    /// with the chosen scoring mode and return the top-`k` entries.
    pub fn search(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
    ) -> Vec<ScoredEntry> {
        let Some(t) = self.table(table) else {
            return Vec::new();
        };
        let index = t.inverted_index.read();
        let analyzer = index.get_search_analyzer(field);
        let analyzed_terms = analyzer.analyze(query);
        if analyzed_terms.is_empty() {
            return Vec::new();
        }

        let posting_lists = index.get_posting_lists_bulk(field, &analyzed_terms);
        let term_doc_freqs: Vec<u64> = posting_lists
            .iter()
            .map(|posting_list| posting_list.len() as u64)
            .collect();
        let stats_arc = Arc::new(search_stats_for_terms(
            index.as_ref(),
            field,
            &analyzed_terms,
            &term_doc_freqs,
        ));
        let scorer = Self::build_text_scorer(mode, stats_arc.clone());
        let term_idfs: Vec<f64> = analyzed_terms
            .iter()
            .zip(&term_doc_freqs)
            .map(|(_, doc_freq)| scorer.idf(*doc_freq))
            .collect();

        let entries = if analyzed_terms.len() == 1 {
            let idf = term_idfs[0];
            let doc_ids: Vec<DocId> = posting_lists[0].iter().map(|entry| entry.doc_id).collect();
            let doc_lengths = index.get_doc_lengths_bulk(&doc_ids, field);
            posting_lists[0]
                .iter()
                .map(|entry| {
                    let term_freq = entry.payload.positions.len() as u64;
                    let doc_length = doc_lengths.get(&entry.doc_id).copied().unwrap_or(0);
                    ScoredEntry {
                        doc_id: entry.doc_id,
                        score: scorer.score_with_idf(term_freq, doc_length, idf),
                    }
                })
                .collect()
        } else {
            let mut candidate_ids = BTreeSet::<DocId>::new();
            let mut present_terms = BTreeMap::<DocId, Vec<(usize, u64)>>::new();
            for (term_index, posting_list) in posting_lists.iter().enumerate() {
                for entry in posting_list {
                    candidate_ids.insert(entry.doc_id);
                    present_terms
                        .entry(entry.doc_id)
                        .or_default()
                        .push((term_index, entry.payload.positions.len() as u64));
                }
            }
            let candidate_ids: Vec<DocId> = candidate_ids.into_iter().collect();
            let doc_lengths = index.get_doc_lengths_bulk(&candidate_ids, field);
            let mut per_term = Vec::with_capacity(analyzed_terms.len());
            candidate_ids
                .into_iter()
                .map(|doc_id| {
                    let doc_length = doc_lengths.get(&doc_id).copied().unwrap_or(0);
                    per_term.clear();
                    for idf in &term_idfs {
                        per_term.push(scorer.score_with_idf(0, doc_length, *idf));
                    }
                    if let Some(terms) = present_terms.get(&doc_id) {
                        for &(term_index, term_freq) in terms {
                            per_term[term_index] =
                                scorer.score_with_idf(term_freq, doc_length, term_idfs[term_index]);
                        }
                    }
                    ScoredEntry {
                        doc_id,
                        score: scorer.combine_scores(&per_term),
                    }
                })
                .collect()
        };
        Self::rank_scored_entries_top_k(entries, top_k)
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
        if self.table(table).is_none() {
            return Err(SQLError::UnknownTable(table.to_string()));
        }
        let doc_ids = self.table_doc_ids(table);
        if labels.len() != doc_ids.len() {
            return Err(SQLError::TypeMismatch(format!(
                "labels length ({}) must match document count ({})",
                labels.len(),
                doc_ids.len()
            )));
        }

        let mode = ScoringMode::BayesianBM25(BayesianBM25Params::default());
        let score_map: std::collections::BTreeMap<DocId, f64> = self
            .search(table, field, query, &mode, usize::MAX)
            .into_iter()
            .map(|entry| (entry.doc_id, entry.score))
            .collect();
        let probabilities: Vec<f64> = doc_ids
            .iter()
            .map(|doc_id| score_map.get(doc_id).copied().unwrap_or(0.0))
            .collect();
        Ok(CalibrationMetrics::report(&probabilities, labels, 10))
    }

    pub fn learn_scoring_params(
        &self,
        table: &str,
        field: &str,
        query: &str,
        labels: &[u8],
    ) -> Result<std::collections::BTreeMap<String, f64>, SQLError> {
        if self.table(table).is_none() {
            return Err(SQLError::UnknownTable(table.to_string()));
        }
        let doc_ids = self.table_doc_ids(table);
        if labels.len() != doc_ids.len() {
            return Err(SQLError::TypeMismatch(format!(
                "labels length ({}) must match document count ({})",
                labels.len(),
                doc_ids.len()
            )));
        }

        let mode = ScoringMode::BayesianBM25(BayesianBM25Params::default());
        let score_map: std::collections::BTreeMap<DocId, f64> = self
            .search(table, field, query, &mode, usize::MAX)
            .into_iter()
            .map(|entry| (entry.doc_id, entry.score))
            .collect();
        let scores: Vec<f64> = doc_ids
            .iter()
            .map(|doc_id| score_map.get(doc_id).copied().unwrap_or(0.0))
            .collect();
        let labels_f: Vec<f64> = labels.iter().map(|label| f64::from(*label)).collect();
        let mut learner = ParameterLearner::default();
        let params = learner.fit_with_options(&scores, &labels_f, None, None);
        let json = serde_json::to_string(&params)
            .map_err(|err| SQLError::Internal(format!("serialize scoring params: {err}")))?;
        self.save_scoring_params(&format!("{table}.{field}"), &json)?;
        Ok(params)
    }

    pub fn update_scoring_params(
        &self,
        table: &str,
        field: &str,
        score: f64,
        label: u8,
    ) -> Result<(), SQLError> {
        let key = format!("{table}.{field}");
        let mut learner = if let Some(json) = self.load_scoring_params(&key) {
            let params: std::collections::BTreeMap<String, f64> =
                serde_json::from_str(&json).unwrap_or_default();
            ParameterLearner::new(
                params.get("alpha").copied().unwrap_or(1.0),
                params.get("beta").copied().unwrap_or(0.0),
                Some(params.get("base_rate").copied().unwrap_or(0.5)),
            )
        } else {
            ParameterLearner::default()
        };
        learner.update(score, f64::from(label), 1.0, 1.0, 0.1);
        let json = serde_json::to_string(&learner.params())
            .map_err(|err| SQLError::Internal(format!("serialize scoring params: {err}")))?;
        self.save_scoring_params(&key, &json)
    }

    /// Top-`k` nearest neighbors against the named vector field.
    pub fn knn_search(
        &self,
        table: &str,
        field: &str,
        query_vector: impl AsRef<[f32]>,
        top_k: usize,
    ) -> Vec<ScoredEntry> {
        if top_k == 0 {
            return Vec::new();
        }
        let Some(t) = self.table(table) else {
            return Vec::new();
        };
        let vector_indexes = t.vector_indexes.read();
        let Some(index) = vector_indexes.get(field) else {
            return Vec::new();
        };
        let pl = index.search_knn(query_vector.as_ref(), top_k);
        Self::rank_top_k(&pl, top_k)
    }

    /// All documents whose cosine similarity to `query_vector` is at least
    /// `threshold`.
    pub fn vector_similarity_search(
        &self,
        table: &str,
        field: &str,
        query_vector: Vec<f32>,
        threshold: f32,
    ) -> Vec<ScoredEntry> {
        let Some((ctx, _)) = self.snapshot_context(table) else {
            return Vec::new();
        };
        let op = VectorSimilarityOperator::new(query_vector, threshold, field);
        let pl = op.execute(&ctx);
        let mut out: Vec<ScoredEntry> = pl.iter().map(ScoredEntry::from_entry).collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        out
    }

    /// Hybrid search: Bayesian BM25 over `text_field` AND KNN over
    /// `vector_field`, combined via log-odds conjunction (Section 4,
    /// Paper 4). Both signals are pre-calibrated to (0, 1) before
    /// fusion: BM25 via the three-term posterior, vector via
    /// `cosine_to_probability`. Returns top-`top_k` by fused score
    /// descending.
    pub fn hybrid_search(&self, params: &HybridSearchParams) -> Vec<ScoredEntry> {
        let Some((ctx, stats_arc)) = self.snapshot_context(params.table) else {
            return Vec::new();
        };
        let analyzer = ctx
            .inverted_index
            .as_ref()
            .expect("snapshot_context populates the inverted index")
            .get_search_analyzer(params.text_field);
        let analyzed_terms = analyzer.analyze(params.text_query);
        if analyzed_terms.is_empty() && !ctx.vector_indexes.contains_key(params.vector_field) {
            return Vec::new();
        }

        let mut signals: Vec<Arc<dyn Operator>> = Vec::new();

        if !analyzed_terms.is_empty() {
            let term_op: Arc<dyn Operator> =
                Arc::new(TermOperator::new(params.text_query, params.text_field));
            let bayes = Arc::new(BayesianBM25Scorer::new(
                BayesianBM25Params::default(),
                stats_arc,
            )) as Arc<dyn Scorer>;
            let scored: Arc<dyn Operator> = Arc::new(ScoreOperator::new(
                bayes,
                term_op,
                analyzed_terms,
                params.text_field,
            ));
            signals.push(scored);
        }

        if ctx.vector_indexes.contains_key(params.vector_field) {
            let knn: Arc<dyn Operator> = Arc::new(KNNOperator::new(
                params.query_vector.clone(),
                params.knn_pool,
                params.vector_field,
            ));
            let cosine_prob: Arc<dyn Operator> = Arc::new(CosineProbabilityOperator::new(knn));
            signals.push(cosine_prob);
        }

        if signals.is_empty() {
            return Vec::new();
        }

        let fusion = LogOddsFusionOperator::new(signals, params.alpha);
        let result = fusion.execute(&ctx);
        Self::rank_top_k(&result, params.top_k)
    }
}
