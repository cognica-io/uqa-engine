//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    ml_deep_learn, DeepLearnOutput, DeepModel, DocId, Engine, ExecutionContext, LearnOptions,
    SQLError, TrainingSet,
};

impl Engine {
    pub fn save_model(&self, name: &str, model: &DeepModel) -> Result<(), SQLError> {
        let json = serde_json::to_string(model)
            .map_err(|e| SQLError::Internal(format!("model serialise: {e}")))?;
        if let Some(catalog) = self.catalog.as_ref() {
            catalog
                .save_model(name, &json)
                .map_err(|e| SQLError::Internal(format!("catalog save_model: {e}")))?;
        }
        self.models.write().insert(name.to_string(), model.clone());
        Ok(())
    }

    pub fn load_model(&self, name: &str) -> Option<DeepModel> {
        if let Some(m) = self.models.read().get(name).cloned() {
            return Some(m);
        }
        let catalog = self.catalog.as_ref()?;
        let json = catalog.load_model(name).ok().flatten()?;
        let model: DeepModel = serde_json::from_str(&json).ok()?;
        self.models.write().insert(name.to_string(), model.clone());
        Some(model)
    }

    pub fn drop_model(&self, name: &str) {
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_model(name);
        }
        self.models.write().remove(name);
    }

    /// compatibility alias for [`Engine::drop_model`].
    pub fn delete_model(&self, name: &str) {
        self.drop_model(name);
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
        if let Some(catalog) = self.catalog.as_ref() {
            catalog
                .save_scoring_params(name, params_json)
                .map_err(|e| SQLError::Internal(format!("catalog save_scoring_params: {e}")))?;
        }
        self.scoring_params
            .write()
            .insert(name.to_string(), params_json.to_string());
        Ok(())
    }

    /// Load persisted scoring parameters for a single signal. Falls
    /// back to the in-memory cache when the engine is not catalog-
    /// backed. Mirrors the canonical UQA implementation's `Engine.load_scoring_params`.
    pub fn load_scoring_params(&self, name: &str) -> Option<String> {
        if let Some(p) = self.scoring_params.read().get(name).cloned() {
            return Some(p);
        }
        if let Some(catalog) = self.catalog.as_ref() {
            if let Ok(Some(json)) = catalog.load_scoring_params(name) {
                self.scoring_params
                    .write()
                    .insert(name.to_string(), json.clone());
                return Some(json);
            }
        }
        None
    }

    /// Snapshot every persisted `(name, params_json)` pair. Mirrors
    /// the canonical UQA implementation's `Engine.load_all_scoring_params`.
    pub fn load_all_scoring_params(&self) -> Vec<(String, String)> {
        if let Some(catalog) = self.catalog.as_ref() {
            if let Ok(rows) = catalog.load_all_scoring_params() {
                let mut cache = self.scoring_params.write();
                for (name, json) in &rows {
                    cache.insert(name.clone(), json.clone());
                }
                return rows;
            }
        }
        let map = self.scoring_params.read();
        let mut out: Vec<_> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Drop persisted scoring parameters for a single signal. Returns
    /// `true` when something was removed.
    pub fn drop_scoring_params(&self, name: &str) -> bool {
        let mut removed = self.scoring_params.write().remove(name).is_some();
        if let Some(catalog) = self.catalog.as_ref() {
            removed = catalog.drop_scoring_params(name).is_ok() || removed;
        }
        removed
    }

    /// Run inference for a saved model against a fresh execution
    /// context. Returns `(doc_id, score)` pairs ordered by `doc_id`.
    pub fn deep_predict(&self, name: &str) -> Option<Vec<(DocId, f64)>> {
        let model = self.load_model(name)?;
        let ctx = ExecutionContext::new();
        let (scores, _) = model.predict(&ctx);
        Some(scores)
    }

    pub fn deep_predict_features(
        &self,
        name: &str,
        examples: &[(DocId, Vec<f64>)],
    ) -> Result<Vec<(DocId, f64)>, SQLError> {
        let model = self
            .load_model(name)
            .ok_or_else(|| SQLError::Unsupported(format!("unknown model {name:?}")))?;
        let (scores, _) = model
            .predict_features(examples)
            .map_err(|e| SQLError::Unsupported(format!("deep_predict: {e}")))?;
        Ok(scores)
    }
}
