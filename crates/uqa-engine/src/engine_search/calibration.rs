//! Bayesian BM25 parameter loading, staleness, sampling, and estimation.

use super::{
    storage_sql_error, BM25Params, BTreeMap, BTreeSet, BayesianBM25Params, Engine, SQLError,
    UnsupervisedBm25ScoreEstimator,
};

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

    pub(super) fn resolve_missing_bayesian_params_in_transaction(
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

    pub(super) fn load_fresh_bayesian_params(
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
    pub(super) fn estimated_params_are_stale(
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
    pub(super) fn sample_calibration_queries(
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
    pub(super) fn auto_estimate_params(
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
}
