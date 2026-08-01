//! Supervised and unsupervised scoring-parameter updates.

use super::{
    storage_sql_error, BM25Params, BTreeMap, DocId, Engine, ParameterLearner, SQLError,
    ScoringMode, UnsupervisedBm25ScoreEstimator,
};

impl Engine {
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

    pub(super) fn learn_scoring_params_inner(
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

    pub(super) fn estimate_scoring_params_inner(
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

    pub(super) fn update_scoring_params_inner(
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
}
