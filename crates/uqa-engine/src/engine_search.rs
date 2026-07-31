//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use super::{
    Arc, BM25Params, BM25Scorer, BayesianBM25Params, BayesianBM25Scorer, CalibrationMetrics,
    CalibrationReport, DocId, Engine, ExecutionContext, HybridSearchParams, InvertedIndex,
    ParameterLearner, PostingList, RawBm25Score, SQLError, ScoredEntry, Scorer, ScoringMode,
    StorageBackendError, StorageBackendResult, UnsupervisedBm25ScoreEstimator,
};
use uqa_core::IndexStats;

fn search_stats_for_terms(
    index: &dyn InvertedIndex,
    field: &str,
    terms: &[String],
    doc_freqs: &[u64],
) -> StorageBackendResult<IndexStats> {
    // The scalar variant skips the vocabulary-wide doc-freq map; every
    // term this query scores gets its frequency set explicitly below.
    let mut stats = index.field_stats_scalar(field)?;

    let mut seen = BTreeSet::<&str>::new();
    for (term, doc_freq) in terms.iter().zip(doc_freqs) {
        if seen.insert(term.as_str()) {
            stats.set_doc_freq(field.to_string(), term.clone(), *doc_freq);
        }
    }
    Ok(stats)
}

fn storage_sql_error(action: &str, error: impl Into<StorageBackendError>) -> SQLError {
    let error = error.into();
    SQLError::Internal(format!("{action}: {error}"))
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
    /// Saved parameters win. Absent or stale auto-estimated parameters
    /// trigger a corpus-driven estimation
    /// that is persisted for subsequent queries, so the raw-score
    /// identity calibration (`alpha = 1, beta = 0`) never silently
    /// ships a score for a populated field. Parameters written by the
    /// online learner carry no `estimated_doc_count` stamp and are
    /// never overwritten automatically.
    pub fn bayesian_params_for(
        &self,
        table: &str,
        field: &str,
    ) -> Result<BayesianBM25Params, SQLError> {
        self.validate_text_search_field(table, field)?;
        if let Some(params) = self.load_fresh_bayesian_params(table, field)? {
            return Ok(params);
        }

        // Estimation is a read/modify/write operation: reserve the durable
        // writer before taking the corpus and parameter snapshots, then
        // recheck in case another session populated a fresh estimate while
        // this caller waited for the writer. The common cached read above
        // deliberately avoids the writer lock.
        self.with_implicit_transaction(|engine| {
            engine.resolve_missing_bayesian_params_in_transaction(table, field)
        })
    }

    /// Resolve calibration while executing an operator tree inside a SQL or
    /// direct-retrieval transaction that already owns the statement gate.
    ///
    /// Rayon branches run on different threads from that gate owner, so they
    /// must not re-enter the public implicit-transaction wrapper. Requiring an
    /// existing transaction keeps this path internal: an unguarded direct API
    /// caller cannot silently join or create a writer transaction.
    pub(crate) fn bayesian_params_for_in_execution(
        &self,
        table: &str,
        field: &str,
    ) -> Result<BayesianBM25Params, SQLError> {
        self.validate_text_search_field(table, field)?;
        if let Some(params) = self.load_fresh_bayesian_params(table, field)? {
            return Ok(params);
        }
        if self.transaction_depth() == 0 {
            return Err(SQLError::Internal(
                "Bayesian calibration execution requires an active statement transaction".into(),
            ));
        }
        self.resolve_missing_bayesian_params_in_transaction(table, field)
    }

    fn resolve_missing_bayesian_params_in_transaction(
        &self,
        table: &str,
        field: &str,
    ) -> Result<BayesianBM25Params, SQLError> {
        self.validate_text_search_field(table, field)?;
        if let Some(params) = self.load_fresh_bayesian_params(table, field)? {
            return Ok(params);
        }
        Ok(self.auto_estimate_params(table, field)?.unwrap_or_default())
    }

    fn load_fresh_bayesian_params(
        &self,
        table: &str,
        field: &str,
    ) -> Result<Option<BayesianBM25Params>, SQLError> {
        let key = format!("{table}.{field}");
        let saved = match self.try_load_scoring_params(&key)? {
            Some(json) => Some(
                serde_json::from_str::<BTreeMap<String, f64>>(&json).map_err(|error| {
                    SQLError::Internal(format!(
                        "decode persisted scoring parameters `{key}`: {error}"
                    ))
                })?,
            ),
            None => None,
        };
        if let Some(params) = saved {
            let stamp = params.get("estimated_doc_count").copied();
            if !self.estimated_params_are_stale(table, stamp)? {
                return Ok(Some(resolve_saved_params(&params)));
            }
        }
        Ok(None)
    }

    /// A stamped estimate goes stale when the corpus grows or shrinks
    /// past [`ESTIMATE_STALENESS_FACTOR`]. Unstamped parameters (online
    /// learner output, hand-written values) never go stale.
    fn estimated_params_are_stale(
        &self,
        table: &str,
        stamp: Option<f64>,
    ) -> Result<bool, SQLError> {
        let Some(stamped_doc_count) = stamp.filter(|value| value.is_finite() && *value > 0.0)
        else {
            return Ok(false);
        };
        let Some(table_state) = self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve search table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let current = table_state
            .inverted_index
            .read()
            .doc_count()
            .map_err(|error| storage_sql_error("read indexed document count", error))?
            as f64;
        Ok(current >= stamped_doc_count * ESTIMATE_STALENESS_FACTOR
            || current <= stamped_doc_count / ESTIMATE_STALENESS_FACTOR)
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
        estimator: &UnsupervisedBm25ScoreEstimator,
    ) -> Result<Vec<Vec<String>>, SQLError> {
        let Some(table_state) = self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve calibration table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let analyzer = table_state.inverted_index.read().get_search_analyzer(field);
        let store = table_state.document_store.read();
        let mut doc_ids = store
            .doc_ids()
            .map_err(|error| storage_sql_error("read calibration document ids", error))?;
        doc_ids.sort_unstable();
        if doc_ids.is_empty() {
            return Ok(Vec::new());
        }

        let lengths = estimator.calibration_lengths();
        let target = estimator.n_samples();
        // Oversample document slots: short documents cannot fill the
        // longer query lengths and get skipped.
        let oversampled_target = target.saturating_mul(2).max(1);
        let stride = (doc_ids.len() / oversampled_target).max(1);
        let mut queries: Vec<Vec<String>> = Vec::new();
        let mut length_index = 0;
        let mut cursor = 0;
        while queries.len() < target && cursor < doc_ids.len() {
            let doc_id = doc_ids[cursor];
            cursor += stride;
            let Some(uqa_core::Value::Str(text)) = store
                .get_field(doc_id, field)
                .map_err(|error| storage_sql_error("read calibration document field", error))?
            else {
                continue;
            };
            let mut distinct: Vec<String> = Vec::new();
            let mut seen = BTreeSet::new();
            for term in analyzer
                .analyze(&text)
                .map_err(|error| storage_sql_error("analyze calibration document field", error))?
            {
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
        Ok(queries)
    }

    /// Estimate unsupervised score-transform parameters from the field's indexed
    /// vocabulary and persist them with a document-count stamp.
    /// Returns `None` (without persisting) when the field has nothing
    /// to sample, so an empty table estimates on first real use.
    fn auto_estimate_params(
        &self,
        table: &str,
        field: &str,
    ) -> Result<Option<BayesianBM25Params>, SQLError> {
        let table_state = self.require_table(table)?;
        let estimator = UnsupervisedBm25ScoreEstimator::default();
        let queries = self.sample_calibration_queries(table, field, &estimator)?;
        let (params, doc_count) = {
            let index = table_state.inverted_index.read();
            if index
                .doc_count()
                .map_err(|error| storage_sql_error("read indexed document count", error))?
                == 0
                || index
                    .vocabulary_terms(field)
                    .map_err(|error| storage_sql_error("read indexed vocabulary", error))?
                    .is_empty()
            {
                return Ok(None);
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
            }
            .map_err(|error| {
                storage_sql_error("estimate BM25 score-transform parameters", error)
            })?;
            let doc_count = index
                .doc_count()
                .map_err(|error| storage_sql_error("read indexed document count", error))?;
            (params, doc_count)
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
            .map_err(|error| SQLError::Internal(format!("serialize scoring params: {error}")))?;
        self.save_scoring_params_inner(&format!("{table}.{field}"), &json)?;
        Ok(Some(params))
    }

    pub(crate) fn snapshot_context(
        &self,
        table: &str,
    ) -> Result<Option<ExecutionContext>, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve snapshot table", error))?
        else {
            return Ok(None);
        };
        let inv = t
            .inverted_index
            .read()
            .snapshot()
            .map_err(|error| storage_sql_error("snapshot inverted index", error))?;
        let docs = t
            .document_store
            .read()
            .snapshot()
            .map_err(|error| storage_sql_error("snapshot document store", error))?;

        let mut ctx = ExecutionContext::new()
            .with_inverted_index(inv)
            .with_document_store(docs);

        for (field, idx) in t.vector_indexes.read().iter() {
            ctx = ctx.with_vector_index(
                field.clone(),
                idx.snapshot()
                    .map_err(|error| storage_sql_error("snapshot vector index", error))?,
            );
        }

        Ok(Some(ctx))
    }

    fn build_text_scorer(
        mode: &ScoringMode,
        stats_arc: Arc<uqa_core::IndexStats>,
        query_term_count: usize,
    ) -> Result<Arc<dyn Scorer>, SQLError> {
        Ok(match mode {
            ScoringMode::BM25(p) => Arc::new(BM25Scorer::new(*p, stats_arc)),
            ScoringMode::BayesianBM25(p) => Arc::new(
                BayesianBM25Scorer::new(p.scaled_for_query_terms(query_term_count), stats_arc)
                    .map_err(|error| SQLError::TypeMismatch(error.to_string()))?,
            ),
        })
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
            .total_cmp(&a.score)
            .then_with(|| a.doc_id.cmp(&b.doc_id))
    }

    fn score_single_text_term(
        index: &dyn InvertedIndex,
        field: &str,
        analyzed_terms: &[String],
        mode: &ScoringMode,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let term = &analyzed_terms[0];
        let mut term_freqs: Vec<(DocId, u64)> = Vec::new();
        index
            .for_each_term_freq(field, term, &mut |doc_id, term_freq| {
                term_freqs.push((doc_id, term_freq));
            })
            .map_err(|error| storage_sql_error("read term frequencies", error))?;
        let document_frequency = u64::try_from(term_freqs.len())
            .map_err(|_| SQLError::Internal("text-search document frequency exceeds u64".into()))?;
        let term_doc_freqs = [document_frequency];
        let stats = Arc::new(
            search_stats_for_terms(index, field, analyzed_terms, &term_doc_freqs)
                .map_err(|error| storage_sql_error("read field statistics", error))?,
        );
        let scorer = Self::build_text_scorer(mode, stats, 1)?;
        let idf = scorer.idf(document_frequency);
        let doc_ids: Vec<DocId> = term_freqs.iter().map(|(doc_id, _)| *doc_id).collect();
        let doc_lengths = index
            .get_doc_lengths_bulk(&doc_ids, field)
            .map_err(|error| storage_sql_error("read document lengths", error))?;
        Ok(term_freqs
            .into_iter()
            .map(|(doc_id, term_freq)| {
                let doc_length = doc_lengths.get(&doc_id).copied().unwrap_or(0);
                ScoredEntry {
                    doc_id,
                    score: scorer
                        .finalize_score(&[scorer.term_score_with_idf(term_freq, doc_length, idf)]),
                }
            })
            .collect())
    }

    fn score_multiple_text_terms(
        index: &dyn InvertedIndex,
        field: &str,
        analyzed_terms: &[String],
        mode: &ScoringMode,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let mut candidate_ids = BTreeSet::<DocId>::new();
        let mut present_terms = BTreeMap::<DocId, Vec<(usize, u64)>>::new();
        let mut term_doc_freqs: Vec<u64> = Vec::with_capacity(analyzed_terms.len());
        for (term_index, term) in analyzed_terms.iter().enumerate() {
            let mut doc_freq = 0_u64;
            let mut frequency_overflow = false;
            index
                .for_each_term_freq(field, term, &mut |doc_id, term_freq| {
                    if let Some(next) = doc_freq.checked_add(1) {
                        doc_freq = next;
                    } else {
                        frequency_overflow = true;
                    }
                    candidate_ids.insert(doc_id);
                    present_terms
                        .entry(doc_id)
                        .or_default()
                        .push((term_index, term_freq));
                })
                .map_err(|error| storage_sql_error("read term frequencies", error))?;
            if frequency_overflow {
                return Err(SQLError::Internal(
                    "text-search document frequency exceeds u64".into(),
                ));
            }
            term_doc_freqs.push(doc_freq);
        }
        let stats = Arc::new(
            search_stats_for_terms(index, field, analyzed_terms, &term_doc_freqs)
                .map_err(|error| storage_sql_error("read field statistics", error))?,
        );
        let scorer = Self::build_text_scorer(mode, stats, analyzed_terms.len())?;
        let term_idfs: Vec<f64> = term_doc_freqs
            .iter()
            .map(|doc_freq| scorer.idf(*doc_freq))
            .collect();
        let candidate_ids: Vec<DocId> = candidate_ids.into_iter().collect();
        let doc_lengths = index
            .get_doc_lengths_bulk(&candidate_ids, field)
            .map_err(|error| storage_sql_error("read document lengths", error))?;
        let mut per_term = Vec::with_capacity(analyzed_terms.len());
        Ok(candidate_ids
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
            .collect())
    }

    /// Physical text-search leaf. Only [`crate::operator_tree_bridge::EngineDriver`]
    /// calls this; public callers enter through [`Self::search`] below.
    pub(crate) fn search_leaf(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve text-search table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let index = t.inverted_index.read();
        let analyzer = index.get_search_analyzer(field);
        let analyzed_terms = analyzer
            .analyze(query)
            .map_err(|error| storage_sql_error("analyze text query", error))?;
        if analyzed_terms.is_empty() {
            return Ok(Vec::new());
        }

        // Walk the postings once per term occurrence, keeping only
        // `(doc_id, term_freq)` and the document frequencies required by the
        // scorer. Single-term queries avoid the per-document term map.
        let entries = if analyzed_terms.len() == 1 {
            Self::score_single_text_term(index.as_ref(), field, &analyzed_terms, mode)?
        } else {
            Self::score_multiple_text_terms(index.as_ref(), field, &analyzed_terms, mode)?
        };
        Ok(Self::rank_scored_entries_top_k(entries, top_k))
    }

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
        let tree = uqa_operators::OperatorTree::Term {
            query: query.to_string(),
            field: Some(field.to_string()),
            scoring: Some(scoring),
        };
        let entries = crate::operator_tree_bridge::execute_scored_tree(self, table, &[], &tree)?;
        Ok(Self::rank_scored_entries_top_k(entries, top_k))
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

    pub fn learn_scoring_params(
        &self,
        table: &str,
        field: &str,
        query: &str,
        labels: &[u8],
    ) -> Result<std::collections::BTreeMap<String, f64>, SQLError> {
        self.with_implicit_transaction(|engine| {
            engine.learn_scoring_params_inner(table, field, query, labels)
        })
    }

    fn learn_scoring_params_inner(
        &self,
        table: &str,
        field: &str,
        query: &str,
        labels: &[u8],
    ) -> Result<std::collections::BTreeMap<String, f64>, SQLError> {
        if self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve scoring table", error))?
            .is_none()
        {
            return Err(SQLError::UnknownTable(table.to_string()));
        }
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

        let mode = ScoringMode::BM25(BM25Params::default());
        let score_map: std::collections::BTreeMap<DocId, f64> = self
            .search(table, field, query, &mode, usize::MAX)?
            .into_iter()
            .map(|entry| (entry.doc_id, entry.score))
            .collect();
        let scores: Vec<f64> = doc_ids
            .iter()
            .map(|doc_id| score_map.get(doc_id).copied().unwrap_or(0.0))
            .collect();
        let labels_f: Vec<f64> = labels.iter().map(|label| f64::from(*label)).collect();
        let mut learner = ParameterLearner::default();
        let params = learner
            .fit_with_options(&scores, &labels_f)
            .map_err(|error| SQLError::Internal(format!("learn scoring parameters: {error}")))?;
        let json = serde_json::to_string(&params)
            .map_err(|err| SQLError::Internal(format!("serialize scoring params: {err}")))?;
        self.save_scoring_params(&format!("{table}.{field}"), &json)?;
        Ok(params)
    }

    /// Estimate and persist an unsupervised BM25 score transform from a
    /// field's indexed vocabulary and raw score distribution.
    pub fn estimate_scoring_params(
        &self,
        table: &str,
        field: &str,
        n_samples: usize,
        tokens_per_query: usize,
        seed: i64,
    ) -> Result<BTreeMap<String, f64>, SQLError> {
        self.with_implicit_transaction(|engine| {
            engine.estimate_scoring_params_inner(table, field, n_samples, tokens_per_query, seed)
        })
    }

    fn estimate_scoring_params_inner(
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
        self.validate_text_search_field(table, field)?;
        let Some(table_state) = self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve scoring table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let estimator = UnsupervisedBm25ScoreEstimator::new(n_samples, tokens_per_query, seed)
            .map_err(|error| SQLError::TypeMismatch(error.to_string()))?;
        let queries = self.sample_calibration_queries(table, field, &estimator)?;
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
            }
            .map_err(|error| storage_sql_error("estimate Bayesian BM25 parameters", error))?;
            let doc_count = index
                .doc_count()
                .map_err(|error| storage_sql_error("read indexed document count", error))?;
            (params, doc_count)
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
        self.with_implicit_transaction(|engine| {
            engine.update_scoring_params_inner(table, field, score, label)
        })
    }

    fn update_scoring_params_inner(
        &self,
        table: &str,
        field: &str,
        score: f64,
        label: u8,
    ) -> Result<(), SQLError> {
        self.validate_text_search_field(table, field)?;
        if !score.is_finite() {
            return Err(SQLError::TypeMismatch(
                "score must be a finite raw BM25 score".into(),
            ));
        }
        if label > 1 {
            return Err(SQLError::TypeMismatch("label must be 0 or 1".into()));
        }
        let key = format!("{table}.{field}");
        let saved = self.load_scoring_params(&key)?;
        let has_saved_params = match saved.as_deref() {
            Some(json) => {
                serde_json::from_str::<BTreeMap<String, f64>>(json).map_err(|error| {
                    SQLError::Internal(format!(
                        "decode persisted scoring parameters `{key}`: {error}"
                    ))
                })?;
                true
            }
            None => false,
        };
        let mut learner = if has_saved_params {
            let current = self.bayesian_params_for(table, field)?;
            let base_rate = (current.base_rate > 0.0).then_some(current.base_rate);
            ParameterLearner::new(current.alpha, current.beta, base_rate).map_err(|error| {
                SQLError::Internal(format!("restore scoring parameter learner: {error}"))
            })?
        } else {
            ParameterLearner::default()
        };
        learner
            .update(score, f64::from(label), 0.1)
            .map_err(|error| SQLError::Internal(format!("update scoring parameters: {error}")))?;
        let json = serde_json::to_string(&learner.params())
            .map_err(|err| SQLError::Internal(format!("serialize scoring params: {err}")))?;
        self.save_scoring_params(&key, &json)
    }

    /// Top-`k` nearest neighbors against the named vector field.
    pub(crate) fn knn_search_leaf(
        &self,
        table: &str,
        field: &str,
        query_vector: impl AsRef<[f32]>,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        let Some(t) = self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve vector-search table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let vector_indexes = t.vector_indexes.read();
        let Some(index) = vector_indexes.get(field) else {
            return Err(SQLError::UnknownColumn(field.to_string()));
        };
        let pl = index
            .search_knn(query_vector.as_ref(), top_k)
            .map_err(|error| storage_sql_error("execute KNN search", error))?;
        Ok(Self::rank_top_k(&pl, top_k))
    }

    /// Top-`k` nearest neighbors through the shared operator optimizer and
    /// executor.
    pub fn knn_search(
        &self,
        table: &str,
        field: &str,
        query_vector: impl AsRef<[f32]>,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let tree = uqa_operators::OperatorTree::KNN {
            query_vector: query_vector.as_ref().to_vec(),
            k: top_k,
            field: field.to_string(),
        };
        let entries = crate::operator_tree_bridge::execute_scored_tree(self, table, &[], &tree)?;
        Ok(Self::rank_scored_entries_top_k(entries, top_k))
    }

    /// Apply a persisted/offline vector calibration model to a KNN pool.
    ///
    /// Unlike `calibrated_vector_match`, this path never fits parameters from
    /// the current query's top-K results. The caller supplies the current
    /// immutable corpus/index/embedding identity in `target`; it must match
    /// the model provenance exactly. The physical table, field, index kind,
    /// dimensions, and candidate K are validated again at execution time.
    pub fn calibrated_vector_search_with_model(
        &self,
        table: &str,
        field: &str,
        query_vector: impl AsRef<[f32]>,
        model: &uqa_scoring::VectorCalibrationModel,
        target: &uqa_scoring::VectorCalibrationTarget,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        model
            .validate_for(target)
            .map_err(|error| SQLError::TypeMismatch(error.to_string()))?;
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| storage_sql_error("resolve calibrated-vector table", error))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let expected_index_id = format!("{table_name}.{field}");
        if target.corpus_id != table_name {
            return Err(SQLError::TypeMismatch(format!(
                "vector calibration corpus_id {:?} does not match table {:?}",
                target.corpus_id, table_name
            )));
        }
        if target.index_id != expected_index_id {
            return Err(SQLError::TypeMismatch(format!(
                "vector calibration index_id {:?} does not match physical index {:?}",
                target.index_id, expected_index_id
            )));
        }

        let table = self
            .try_table(&table_name)
            .map_err(|error| storage_sql_error("resolve calibrated-vector table", error))?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let indexes = table.vector_indexes.read();
        let index = indexes
            .get(field)
            .ok_or_else(|| SQLError::UnknownColumn(field.to_string()))?;
        if target.index_kind != index.index_kind() {
            return Err(SQLError::TypeMismatch(format!(
                "vector calibration index kind {:?} does not match {:?}",
                target.index_kind,
                index.index_kind()
            )));
        }
        if target.dimensions != index.dimensions() {
            return Err(SQLError::VectorDimMismatch {
                expected: index.dimensions() as usize,
                actual: target.dimensions as usize,
            });
        }
        let raw = index
            .search_knn(query_vector.as_ref(), target.candidate_k)
            .map_err(|error| storage_sql_error("execute calibrated-vector KNN", error))?;
        let mut calibrated = Vec::with_capacity(raw.len());
        for entry in &raw {
            if !entry.payload.score.is_finite() || !(-1.0..=1.0).contains(&entry.payload.score) {
                return Err(SQLError::Internal(format!(
                    "calibrated-vector KNN returned invalid cosine score {} for document {}",
                    entry.payload.score, entry.doc_id
                )));
            }
            let probability = model
                .calibrate_one(1.0 - entry.payload.score, target)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            calibrated.push(ScoredEntry {
                doc_id: entry.doc_id,
                score: probability,
            });
        }
        Ok(Self::rank_scored_entries_top_k(
            calibrated,
            target.candidate_k,
        ))
    }

    /// All documents whose cosine similarity to `query_vector` is at least
    /// `threshold`.
    pub fn vector_similarity_search(
        &self,
        table: &str,
        field: &str,
        query_vector: Vec<f32>,
        threshold: f32,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let tree = uqa_operators::OperatorTree::VectorSimilarity {
            query_vector,
            threshold,
            field: field.to_string(),
        };
        let mut out = crate::operator_tree_bridge::execute_scored_tree(self, table, &[], &tree)?;
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        Ok(out)
    }

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
