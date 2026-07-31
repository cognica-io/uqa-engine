//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    ml_deep_learn, DeepLearnOutput, DeepModel, DocId, Engine, ExecutionContext, LearnOptions,
    SQLError, TrainingSet,
};

const VECTOR_CALIBRATION_MODEL_PREFIX: &str = "vector_calibration_model::";

fn vector_calibration_model_key(name: &str) -> Result<String, SQLError> {
    if name.trim().is_empty() {
        return Err(SQLError::TypeMismatch(
            "vector calibration model name must not be empty".into(),
        ));
    }
    Ok(format!("{VECTOR_CALIBRATION_MODEL_PREFIX}{name}"))
}

impl Engine {
    pub fn save_model(&self, name: &str, model: &DeepModel) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| engine.save_model_inner(name, model))
    }

    fn save_model_inner(&self, name: &str, model: &DeepModel) -> Result<(), SQLError> {
        let json = serde_json::to_string(model)
            .map_err(|e| SQLError::Internal(format!("model serialise: {e}")))?;
        let mut models = self.models.write();
        if let Some(catalog) = self.catalog.as_ref() {
            catalog
                .save_model(name, &json)
                .map_err(|e| SQLError::Internal(format!("catalog save_model: {e}")))?;
        }
        models.insert(name.to_string(), model.clone());
        drop(models);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub fn load_model(&self, name: &str) -> Result<Option<DeepModel>, SQLError> {
        let Some(catalog) = self.catalog.as_ref() else {
            return Ok(self.models.read().get(name).cloned());
        };
        let json = catalog
            .load_model(name)
            .map_err(|err| SQLError::Internal(format!("catalog load_model: {err}")))?;
        let model = json
            .as_deref()
            .map(serde_json::from_str::<DeepModel>)
            .transpose()
            .map_err(|err| SQLError::Internal(format!("catalog model decode: {err}")))?;
        let mut cache = self.models.write();
        match model.as_ref() {
            Some(model) => {
                cache.insert(name.to_string(), model.clone());
            }
            None => {
                cache.remove(name);
            }
        }
        Ok(model)
    }

    pub fn drop_model(&self, name: &str) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| engine.drop_model_inner(name))
    }

    fn drop_model_inner(&self, name: &str) -> Result<bool, SQLError> {
        if self.load_model(name)?.is_none() {
            return Ok(false);
        }
        let mut models = self.models.write();
        if let Some(catalog) = self.catalog.as_ref() {
            catalog
                .drop_model(name)
                .map_err(|err| SQLError::Internal(format!("catalog drop_model: {err}")))?;
        }
        models.remove(name);
        drop(models);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// compatibility alias for [`Engine::drop_model`].
    pub fn delete_model(&self, name: &str) -> Result<bool, SQLError> {
        self.drop_model(name)
    }

    /// Train an analytical deep model and persist it under `name`.
    pub fn deep_learn(
        &self,
        name: &str,
        training_set: &TrainingSet,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError> {
        let output = ml_deep_learn(training_set, options)
            .map_err(|e| SQLError::Unsupported(format!("deep_learn: {e}")))?;
        self.save_model(name, &output.model)?;
        Ok(output)
    }

    /// Parse a JSON [`TrainingSet`], train it, and persist the model.
    pub fn deep_learn_json(
        &self,
        name: &str,
        training_json: &str,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError> {
        let training_set: TrainingSet = serde_json::from_str(training_json).map_err(|e| {
            SQLError::TypeMismatch(format!("invalid deep_learn training JSON: {e}"))
        })?;
        self.deep_learn(name, &training_set, options)
    }

    /// Train from a table containing `features` and `label` columns.
    pub fn deep_learn_table(
        &self,
        name: &str,
        table: &str,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError> {
        let training_set = self.training_set_from_table(table, "features", "label")?;
        self.deep_learn(name, &training_set, options)
    }

    /// Persist Bayesian calibration parameters for a named signal. The
    /// parameters arrive serialised as a JSON string so callers can
    /// stuff arbitrary `(alpha, beta, base_rate, ...)` shapes through
    /// without forcing a struct. Mirrors the canonical UQA implementation's `save_scoring_params`.
    pub fn save_scoring_params(&self, name: &str, params_json: &str) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| engine.save_scoring_params_inner(name, params_json))
    }

    pub(crate) fn save_scoring_params_inner(
        &self,
        name: &str,
        params_json: &str,
    ) -> Result<(), SQLError> {
        let mut scoring_params = self.scoring_params.write();
        if let Some(catalog) = self.catalog.as_ref() {
            catalog
                .save_scoring_params(name, params_json)
                .map_err(|e| SQLError::Internal(format!("catalog save_scoring_params: {e}")))?;
        }
        scoring_params.insert(name.to_string(), params_json.to_string());
        drop(scoring_params);
        self.note_catalog_registry_changed();
        Ok(())
    }

    /// Load persisted scoring parameters for a single signal. Falls
    /// back to the in-memory cache when the engine is not catalog-
    /// backed. Mirrors the canonical UQA implementation's `Engine.load_scoring_params`.
    pub fn load_scoring_params(&self, name: &str) -> Result<Option<String>, SQLError> {
        let Some(catalog) = self.catalog.as_ref() else {
            return Ok(self.scoring_params.read().get(name).cloned());
        };
        let value = catalog
            .load_scoring_params(name)
            .map_err(|err| SQLError::Internal(format!("catalog load_scoring_params: {err}")))?;
        let mut cache = self.scoring_params.write();
        match value.as_ref() {
            Some(json) => {
                cache.insert(name.to_string(), json.clone());
            }
            None => {
                cache.remove(name);
            }
        }
        Ok(value)
    }

    /// Fallible scoring-parameter read. Persistent sessions consult their
    /// own catalog connection instead of trusting an engine-local snapshot;
    /// this preserves transaction isolation and makes commits from sibling
    /// sessions visible without reopening the engine.
    pub fn try_load_scoring_params(&self, name: &str) -> Result<Option<String>, SQLError> {
        self.load_scoring_params(name)
    }

    /// Snapshot every persisted `(name, params_json)` pair. Mirrors
    /// the canonical UQA implementation's `Engine.load_all_scoring_params`.
    pub fn load_all_scoring_params(&self) -> Result<Vec<(String, String)>, SQLError> {
        let mut out = if let Some(catalog) = self.catalog.as_ref() {
            let rows = catalog.load_all_scoring_params().map_err(|err| {
                SQLError::Internal(format!("catalog load_all_scoring_params: {err}"))
            })?;
            let mut cache = self.scoring_params.write();
            cache.clear();
            cache.extend(rows.iter().cloned());
            rows
        } else {
            self.scoring_params
                .read()
                .iter()
                .map(|(name, json)| (name.clone(), json.clone()))
                .collect()
        };
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Drop persisted scoring parameters for a single signal. Returns
    /// `true` when something was removed.
    pub fn drop_scoring_params(&self, name: &str) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| engine.drop_scoring_params_inner(name))
    }

    /// Persist a typed vector-calibration model, including the corpus, index,
    /// embedding-model, candidate-K, and version provenance that constrains
    /// its safe reuse.
    pub fn save_vector_calibration_model(
        &self,
        name: &str,
        model: &uqa_scoring::VectorCalibrationModel,
    ) -> Result<(), SQLError> {
        let key = vector_calibration_model_key(name)?;
        let json = model
            .to_json()
            .map_err(|error| SQLError::TypeMismatch(error.to_string()))?;
        self.save_scoring_params(&key, &json)
    }

    /// Load and validate a persisted vector-calibration model. Unsupported
    /// schema versions and invalid numeric parameters are errors rather than
    /// silently falling back to query-pool calibration.
    pub fn load_vector_calibration_model(
        &self,
        name: &str,
    ) -> Result<Option<uqa_scoring::VectorCalibrationModel>, SQLError> {
        let key = vector_calibration_model_key(name)?;
        self.load_scoring_params(&key)?
            .as_deref()
            .map(uqa_scoring::VectorCalibrationModel::from_json)
            .transpose()
            .map_err(|error| SQLError::TypeMismatch(error.to_string()))
    }

    pub fn drop_vector_calibration_model(&self, name: &str) -> Result<bool, SQLError> {
        let key = vector_calibration_model_key(name)?;
        self.drop_scoring_params(&key)
    }

    fn drop_scoring_params_inner(&self, name: &str) -> Result<bool, SQLError> {
        if self.load_scoring_params(name)?.is_none() {
            return Ok(false);
        }
        let mut scoring_params = self.scoring_params.write();
        if let Some(catalog) = self.catalog.as_ref() {
            catalog
                .drop_scoring_params(name)
                .map_err(|err| SQLError::Internal(format!("catalog drop_scoring_params: {err}")))?;
        }
        scoring_params.remove(name);
        drop(scoring_params);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    /// Run inference for a saved model against a fresh execution
    /// context. Returns `(doc_id, score)` pairs ordered by `doc_id`.
    pub(crate) fn deep_predict_leaf(
        &self,
        name: &str,
    ) -> Result<Option<Vec<(DocId, f64)>>, SQLError> {
        let Some(model) = self.load_model(name)? else {
            return Ok(None);
        };
        let ctx = ExecutionContext::new();
        let (scores, _) = model
            .predict(&ctx)
            .map_err(|error| SQLError::Internal(format!("deep prediction failed: {error}")))?;
        Ok(Some(scores))
    }

    /// Run saved-model inference through the shared operator optimizer and
    /// plan executor. A missing model retains the public API's `None`
    /// contract; the physical driver only receives known models.
    pub fn deep_predict(&self, name: &str) -> Result<Option<Vec<(DocId, f64)>>, SQLError> {
        if self.load_model(name)?.is_none() {
            return Ok(None);
        }
        let tree = uqa_operators::OperatorTree::DeepPredict {
            model: name.to_string(),
        };
        let entries = crate::operator_tree_bridge::execute_scored_tree(self, "", &[], &tree)?;
        Ok(Some(
            entries
                .into_iter()
                .map(|entry| (entry.doc_id, entry.score))
                .collect(),
        ))
    }

    pub fn deep_predict_features(
        &self,
        name: &str,
        examples: &[(DocId, Vec<f64>)],
    ) -> Result<Vec<(DocId, f64)>, SQLError> {
        let model = self
            .load_model(name)?
            .ok_or_else(|| SQLError::Unsupported(format!("unknown model {name:?}")))?;
        let (scores, _) = model
            .predict_features(examples)
            .map_err(|e| SQLError::Unsupported(format!("deep_predict: {e}")))?;
        Ok(scores)
    }
}
