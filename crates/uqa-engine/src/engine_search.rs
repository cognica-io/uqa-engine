//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use super::{
    Arc, BM25Params, BM25Scorer, BayesianBM25Params, BayesianBM25Scorer, BayesianScoreEstimator,
    CalibratedVectorOperator, CalibrationMetrics, CalibrationReport, DocId, Engine,
    ExecutionContext, HybridSearchParams, InvertedIndex, LogOddsFusionOperator, Operator,
    ParameterLearner, PostingList, SQLError, ScoreOperator, ScoredEntry, Scorer, ScoringMode,
    TermOperator, VectorSimilarityOperator,
};
use uqa_core::IndexStats;

fn search_stats_for_terms(
    index: &dyn InvertedIndex,
    field: &str,
    terms: &[String],
    doc_freqs: &[u64],
) -> IndexStats {
    // The scalar variant skips the vocabulary-wide doc-freq map; every
    // term this query scores gets its frequency set explicitly below.
    let mut stats = index.field_stats_scalar(field);

    let mut seen = BTreeSet::<&str>::new();
    for (term, doc_freq) in terms.iter().zip(doc_freqs) {
        if seen.insert(term.as_str()) {
            stats.set_doc_freq(field.to_string(), term.clone(), *doc_freq);
        }
    }
    stats
}

/// Auto-estimated parameters are refreshed once the corpus doubles or
/// halves relative to the document count stamped at estimation time.
const ESTIMATE_STALENESS_FACTOR: f64 = 2.0;

fn resolve_saved_params(params: &BTreeMap<String, f64>) -> BayesianBM25Params {
    let mut resolved = BayesianBM25Params::default();
    if let Some(alpha) = params
        .get("alpha")
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        resolved.alpha = alpha;
    }
    if let Some(beta) = params
        .get("beta")
        .copied()
        .filter(|value| value.is_finite())
    {
        resolved.beta = beta;
    }
    if let Some(base_rate) = params
        .get("base_rate")
        .copied()
        .filter(|value| value.is_finite() && (0.0..1.0).contains(value))
    {
        resolved.base_rate = base_rate;
    }
    if let Some(calibration_tokens) = params
        .get("calibration_tokens")
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        resolved.calibration_tokens = calibration_tokens;
        if let Some(beta_slope) = params
            .get("beta_slope")
            .copied()
            .filter(|value| value.is_finite())
        {
            resolved.beta_slope = beta_slope;
        }
        if let Some(sigma_slope) = params
            .get("sigma_slope")
            .copied()
            .filter(|value| value.is_finite())
        {
            resolved.sigma_slope = sigma_slope;
        }
    }
    resolved
}

impl Engine {
    /// Resolve the Bayesian BM25 calibration for `table.field`.
    ///
    /// Saved parameters win. Absent, unparseable, or stale
    /// auto-estimated parameters trigger a corpus-driven estimation
    /// that is persisted for subsequent queries, so the raw-score
    /// identity calibration (`alpha = 1, beta = 0`) never silently
    /// ships a score for a populated field. Parameters written by the
    /// online learner carry no `estimated_doc_count` stamp and are
    /// never overwritten automatically.
    pub(crate) fn bayesian_params_for(&self, table: &str, field: &str) -> BayesianBM25Params {
        let saved = self
            .load_scoring_params(&format!("{table}.{field}"))
            .and_then(|json| serde_json::from_str::<BTreeMap<String, f64>>(&json).ok());
        if let Some(params) = saved {
            let stamp = params.get("estimated_doc_count").copied();
            if !self.estimated_params_are_stale(table, stamp) {
                return resolve_saved_params(&params);
            }
        }
        self.auto_estimate_params(table, field).unwrap_or_default()
    }

    /// A stamped estimate goes stale when the corpus grows or shrinks
    /// past [`ESTIMATE_STALENESS_FACTOR`]. Unstamped parameters (online
    /// learner output, hand-written values) never go stale.
    fn estimated_params_are_stale(&self, table: &str, stamp: Option<f64>) -> bool {
        let Some(stamped_doc_count) = stamp.filter(|value| value.is_finite() && *value > 0.0)
        else {
            return false;
        };
        let Some(table_state) = self.table(table) else {
            return false;
        };
        let current = table_state.inverted_index.read().doc_count() as f64;
        current >= stamped_doc_count * ESTIMATE_STALENESS_FACTOR
            || current <= stamped_doc_count / ESTIMATE_STALENESS_FACTOR
    }

    /// Build coherent calibration pseudo-queries by sampling stored
    /// documents: each query takes the leading distinct analyzed terms
    /// of one document, so its terms co-occur the way real query terms
    /// do in relevant documents. Vocabulary-random sampling cannot
    /// model that co-occurrence, which flattens the fitted length
    /// slopes on sparse-vocabulary corpora. Documents are
    /// stride-sampled, so the result is deterministic per corpus.
    fn sample_calibration_queries(
        &self,
        table: &str,
        field: &str,
        estimator: &BayesianScoreEstimator,
    ) -> Vec<Vec<String>> {
        let Some(table_state) = self.table(table) else {
            return Vec::new();
        };
        let analyzer = table_state.inverted_index.read().get_search_analyzer(field);
        let store = table_state.document_store.read();
        let mut doc_ids: Vec<DocId> = store.doc_ids().into_iter().collect();
        doc_ids.sort_unstable();
        if doc_ids.is_empty() {
            return Vec::new();
        }

        let lengths = estimator.calibration_lengths();
        let target = estimator.n_samples();
        // Oversample document slots: short documents cannot fill the
        // longer query lengths and get skipped.
        let stride = (doc_ids.len() / (target * 2).max(1)).max(1);
        let mut queries: Vec<Vec<String>> = Vec::new();
        let mut length_index = 0;
        let mut cursor = 0;
        while queries.len() < target && cursor < doc_ids.len() {
            let doc_id = doc_ids[cursor];
            cursor += stride;
            let Some(uqa_core::Value::Str(text)) = store.get_field(doc_id, field) else {
                continue;
            };
            let mut distinct: Vec<String> = Vec::new();
            let mut seen = BTreeSet::new();
            for term in analyzer.analyze(&text) {
                if seen.insert(term.clone()) {
                    distinct.push(term);
                }
            }
            let length = lengths[length_index % lengths.len()];
            if distinct.len() < length {
                continue;
            }
            distinct.truncate(length);
            queries.push(distinct);
            length_index += 1;
        }
        queries
    }

    /// Estimate calibration parameters from the field's indexed
    /// vocabulary and persist them with a document-count stamp.
    /// Returns `None` (without persisting) when the field has nothing
    /// to sample, so an empty table estimates on first real use.
    fn auto_estimate_params(&self, table: &str, field: &str) -> Option<BayesianBM25Params> {
        let table_state = self.table(table)?;
        let estimator = BayesianScoreEstimator::default();
        let queries = self.sample_calibration_queries(table, field, &estimator);
        let (params, doc_count) = {
            let index = table_state.inverted_index.read();
            if index.doc_count() == 0 || index.vocabulary_terms(field).is_empty() {
                return None;
            }
            let params = if queries.is_empty() {
                estimator.estimate(index.as_ref(), field, BM25Params::default())
            } else {
                estimator.estimate_with_queries(
                    index.as_ref(),
                    field,
                    BM25Params::default(),
                    &queries,
                )
            };
            (params, index.doc_count())
        };
        let values = BTreeMap::from([
            ("alpha".to_string(), params.alpha),
            ("beta".to_string(), params.beta),
            ("base_rate".to_string(), params.base_rate),
            ("calibration_tokens".to_string(), params.calibration_tokens),
            ("beta_slope".to_string(), params.beta_slope),
            ("sigma_slope".to_string(), params.sigma_slope),
            ("estimated_doc_count".to_string(), doc_count as f64),
        ]);
        let json = serde_json::to_string(&values).ok()?;
        self.save_scoring_params(&format!("{table}.{field}"), &json)
            .ok()?;
        Some(params)
    }

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
        query_term_count: usize,
    ) -> Arc<dyn Scorer> {
        match mode {
            ScoringMode::BM25(p) => Arc::new(BM25Scorer::new(*p, stats_arc)),
            ScoringMode::BayesianBM25(p) => Arc::new(BayesianBM25Scorer::new(
                p.scaled_for_query_terms(query_term_count),
                stats_arc,
            )),
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

        // Walk the postings once per term occurrence, keeping only
        // `(doc_id, term_freq)` - no owned posting lists, so no payload
        // clone per matching document. The walk also yields each term's
        // document frequency, which the scorer needs before any scoring
        // happens.
        let entries = if analyzed_terms.len() == 1 {
            let term = &analyzed_terms[0];
            let mut term_freqs: Vec<(DocId, u64)> = Vec::new();
            index.for_each_posting(field, term, &mut |entry| {
                term_freqs.push((entry.doc_id, entry.payload.positions.len() as u64));
            });
            let term_doc_freqs = [term_freqs.len() as u64];
            let stats_arc = Arc::new(search_stats_for_terms(
                index.as_ref(),
                field,
                &analyzed_terms,
                &term_doc_freqs,
            ));
            let scorer = Self::build_text_scorer(mode, stats_arc, 1);
            let idf = scorer.idf(term_doc_freqs[0]);

            let doc_ids: Vec<DocId> = term_freqs.iter().map(|(doc_id, _)| *doc_id).collect();
            let doc_lengths = index.get_doc_lengths_bulk(&doc_ids, field);
            term_freqs
                .into_iter()
                .map(|(doc_id, term_freq)| {
                    let doc_length = doc_lengths.get(&doc_id).copied().unwrap_or(0);
                    ScoredEntry {
                        doc_id,
                        score: scorer.finalize_score(&[
                            scorer.term_score_with_idf(term_freq, doc_length, idf)
                        ]),
                    }
                })
                .collect()
        } else {
            let mut candidate_ids = BTreeSet::<DocId>::new();
            let mut present_terms = BTreeMap::<DocId, Vec<(usize, u64)>>::new();
            let mut term_doc_freqs: Vec<u64> = Vec::with_capacity(analyzed_terms.len());
            for (term_index, term) in analyzed_terms.iter().enumerate() {
                let mut doc_freq = 0_u64;
                index.for_each_posting(field, term, &mut |entry| {
                    doc_freq += 1;
                    candidate_ids.insert(entry.doc_id);
                    present_terms
                        .entry(entry.doc_id)
                        .or_default()
                        .push((term_index, entry.payload.positions.len() as u64));
                });
                term_doc_freqs.push(doc_freq);
            }
            let stats_arc = Arc::new(search_stats_for_terms(
                index.as_ref(),
                field,
                &analyzed_terms,
                &term_doc_freqs,
            ));
            let scorer = Self::build_text_scorer(mode, stats_arc, analyzed_terms.len());
            let term_idfs: Vec<f64> = term_doc_freqs
                .iter()
                .map(|doc_freq| scorer.idf(*doc_freq))
                .collect();

            let candidate_ids: Vec<DocId> = candidate_ids.into_iter().collect();
            let doc_lengths = index.get_doc_lengths_bulk(&candidate_ids, field);
            let mut per_term = Vec::with_capacity(analyzed_terms.len());
            candidate_ids
                .into_iter()
                .map(|doc_id| {
                    let doc_length = doc_lengths.get(&doc_id).copied().unwrap_or(0);
                    per_term.clear();
                    for idf in &term_idfs {
                        per_term.push(scorer.term_score_with_idf(0, doc_length, *idf));
                    }
                    if let Some(terms) = present_terms.get(&doc_id) {
                        for &(term_index, term_freq) in terms {
                            per_term[term_index] = scorer.term_score_with_idf(
                                term_freq,
                                doc_length,
                                term_idfs[term_index],
                            );
                        }
                    }
                    ScoredEntry {
                        doc_id,
                        score: scorer.finalize_score(&per_term),
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
        if labels.iter().any(|label| *label > 1) {
            return Err(SQLError::TypeMismatch(
                "labels must contain only 0 or 1".into(),
            ));
        }

        let mode = ScoringMode::BayesianBM25(self.bayesian_params_for(table, field));
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
        if labels.iter().any(|label| *label > 1) {
            return Err(SQLError::TypeMismatch(
                "labels must contain only 0 or 1".into(),
            ));
        }

        let mode = ScoringMode::BM25(BM25Params::default());
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
        let params = learner.fit_with_options(&scores, &labels_f);
        let json = serde_json::to_string(&params)
            .map_err(|err| SQLError::Internal(format!("serialize scoring params: {err}")))?;
        self.save_scoring_params(&format!("{table}.{field}"), &json)?;
        Ok(params)
    }

    /// Estimate and persist Lucene-style Bayesian BM25 calibration
    /// parameters from a field's indexed vocabulary and raw score
    /// distribution.
    pub fn estimate_scoring_params(
        &self,
        table: &str,
        field: &str,
        n_samples: usize,
        tokens_per_query: usize,
        seed: i64,
    ) -> Result<BTreeMap<String, f64>, SQLError> {
        if n_samples == 0 || tokens_per_query == 0 {
            return Err(SQLError::TypeMismatch(
                "n_samples and tokens_per_query must be positive".into(),
            ));
        }
        if n_samples.checked_mul(tokens_per_query).is_none() {
            return Err(SQLError::TypeMismatch(
                "n_samples * tokens_per_query exceeds usize".into(),
            ));
        }
        let Some(table_state) = self.table(table) else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let estimator = BayesianScoreEstimator::new(n_samples, tokens_per_query, seed);
        let queries = self.sample_calibration_queries(table, field, &estimator);
        let (params, doc_count) = {
            let index = table_state.inverted_index.read();
            let params = if queries.is_empty() {
                estimator.estimate(index.as_ref(), field, BM25Params::default())
            } else {
                estimator.estimate_with_queries(
                    index.as_ref(),
                    field,
                    BM25Params::default(),
                    &queries,
                )
            };
            (params, index.doc_count())
        };
        let values = BTreeMap::from([
            ("alpha".to_string(), params.alpha),
            ("beta".to_string(), params.beta),
            ("base_rate".to_string(), params.base_rate),
            ("calibration_tokens".to_string(), params.calibration_tokens),
            ("beta_slope".to_string(), params.beta_slope),
            ("sigma_slope".to_string(), params.sigma_slope),
            ("estimated_doc_count".to_string(), doc_count as f64),
        ]);
        let json = serde_json::to_string(&values)
            .map_err(|err| SQLError::Internal(format!("serialize scoring params: {err}")))?;
        self.save_scoring_params(&format!("{table}.{field}"), &json)?;
        Ok(values)
    }

    pub fn update_scoring_params(
        &self,
        table: &str,
        field: &str,
        score: f64,
        label: u8,
    ) -> Result<(), SQLError> {
        if self.table(table).is_none() {
            return Err(SQLError::UnknownTable(table.to_string()));
        }
        if !score.is_finite() {
            return Err(SQLError::TypeMismatch(
                "score must be a finite raw BM25 score".into(),
            ));
        }
        if label > 1 {
            return Err(SQLError::TypeMismatch("label must be 0 or 1".into()));
        }
        let key = format!("{table}.{field}");
        let saved_params_are_valid = self
            .load_scoring_params(&key)
            .and_then(|json| serde_json::from_str::<BTreeMap<String, f64>>(&json).ok())
            .is_some();
        let mut learner = if saved_params_are_valid {
            let current = self.bayesian_params_for(table, field);
            let base_rate = (current.base_rate > 0.0).then_some(current.base_rate);
            ParameterLearner::new(current.alpha, current.beta, base_rate)
        } else {
            ParameterLearner::default()
        };
        learner.update(score, f64::from(label), 0.1);
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

    /// Hybrid search under the probability contract: query-level
    /// Bayesian BM25 over `text_field` and likelihood-ratio calibrated
    /// KNN over `vector_field`, combined with sparse log-odds fusion.
    /// Both signals enter as prior-free evidence -- BM25 by one sigmoid
    /// over the complete raw query score with the prior stripped, and
    /// cosine distance by the pool-fitted two-Gaussian calibration --
    /// while the text field's estimated relevance prior enters the
    /// fusion exactly once. Signals are weighted per query by their
    /// gated-evidence spread over the candidate pool, so a signal that
    /// cannot separate the candidates loses influence instead of
    /// diluting the fused ranking. Returns the top-`top_k` entries by
    /// descending fused probability.
    pub fn hybrid_search(&self, params: &HybridSearchParams) -> Vec<ScoredEntry> {
        let Some((ctx, _)) = self.snapshot_context(params.table) else {
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
        let mut fusion_base_rate = None;

        if !analyzed_terms.is_empty() {
            let stats_arc = Arc::new(
                ctx.inverted_index
                    .as_ref()
                    .expect("snapshot_context populates the inverted index")
                    .field_stats(params.text_field),
            );
            let term_op: Arc<dyn Operator> =
                Arc::new(TermOperator::new(params.text_query, params.text_field));
            let calibration = self
                .bayesian_params_for(params.table, params.text_field)
                .scaled_for_query_terms(analyzed_terms.len());
            if calibration.base_rate > 0.0 {
                fusion_base_rate = Some(calibration.base_rate);
            }
            let bayes = Arc::new(BayesianBM25Scorer::new(
                calibration.evidence_params(),
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
            let calibrated: Arc<dyn Operator> = Arc::new(CalibratedVectorOperator::new(
                params.query_vector.clone(),
                params.knn_pool,
                params.vector_field,
            ));
            signals.push(calibrated);
        }

        if signals.is_empty() {
            return Vec::new();
        }

        let mut fusion = LogOddsFusionOperator::new(signals, params.alpha).with_adaptive_weights();
        if let Some(base_rate) = fusion_base_rate {
            fusion = fusion.with_base_rate(base_rate);
        }
        let result = fusion.execute(&ctx);
        Self::rank_top_k(&result, params.top_k)
    }
}
