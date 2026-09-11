//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Model training invocation and scalar report materialization.

use std::collections::BTreeMap;
use uqa_core::{DocId, Value};
use uqa_ml::{DeepLearnOutput, DeepModel, LearnOptions, TrainingSet};
use uqa_sql::{
    semantics::{
        runtime_scalars::{deep_learn_arguments, deep_learn_source, DeepLearnSource},
        source_filters::checked_integer_value,
    },
    SQLError, ScalarExpr,
};
use uqa_storage::{document_store::Document, StorageBackendResult};
mod data;

/// A retained table generation whose document-store read guard covers the ID scan.
pub trait TrainingTable {
    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>>;
}

/// Session-bound physical reads, including generated-column materialization.
pub trait TrainingTables {
    fn training_table(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<Box<dyn TrainingTable + '_>>>;
    fn training_documents(
        &self,
        table: &str,
        doc_ids: &[DocId],
        projection: &[String],
    ) -> Result<BTreeMap<DocId, Document>, SQLError>;
}

/// Model persistence enters the caller's existing implicit transaction boundary.
pub trait TrainedModels {
    fn save_model(&self, name: &str, model: &DeepModel) -> Result<(), SQLError>;
}

pub struct ModelTrainingContext<'a> {
    pub tables: &'a dyn TrainingTables,
    pub models: &'a dyn TrainedModels,
}

impl ModelTrainingContext<'_> {
    pub fn train(
        &self,
        name: &str,
        training_set: &TrainingSet,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError> {
        let output = uqa_ml::deep_learn(training_set, options)
            .map_err(|e| SQLError::Unsupported(format!("deep_learn: {e}")))?;
        self.models.save_model(name, &output.model)?;
        Ok(output)
    }

    pub fn train_json(
        &self,
        name: &str,
        training_json: &str,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError> {
        let training_set: TrainingSet = serde_json::from_str(training_json).map_err(|e| {
            SQLError::TypeMismatch(format!("invalid deep_learn training JSON: {e}"))
        })?;
        self.train(name, &training_set, options)
    }

    pub fn train_table(
        &self,
        name: &str,
        table: &str,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError> {
        let training_set = data::training_set_from_table(self.tables, table, "features", "label")?;
        self.train(name, &training_set, options)
    }
}

pub fn run_deep_learn_projection(
    models: &ModelTrainingContext<'_>,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    let (model_name, training_source) = deep_learn_arguments(args, evaluate)?;
    let output = match deep_learn_source(&training_source) {
        DeepLearnSource::Json(source) => {
            models.train_json(&model_name, source, &LearnOptions::default())?
        }
        DeepLearnSource::Table(source) => {
            models.train_table(&model_name, source, &LearnOptions::default())?
        }
    };
    let mut report = BTreeMap::new();
    report.insert("model".into(), Value::Str(model_name));
    report.insert(
        "examples".into(),
        checked_integer_value(output.report.examples, "training example count")?,
    );
    report.insert(
        "feature_dimensions".into(),
        checked_integer_value(output.report.feature_dimensions, "feature dimension count")?,
    );
    report.insert(
        "class_count".into(),
        checked_integer_value(output.report.class_count, "class count")?,
    );
    Ok(Value::Map(report))
}

#[cfg(test)]
mod tests;
