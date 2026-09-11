//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Model training invocation and scalar report materialization.

use std::collections::BTreeMap;
use uqa_core::Value;
use uqa_ml::{DeepLearnOutput, LearnOptions};
use uqa_sql::{
    semantics::{
        runtime_scalars::{deep_learn_arguments, deep_learn_source, DeepLearnSource},
        source_filters::checked_integer_value,
    },
    SQLError, ScalarExpr,
};
pub trait ModelTraining {
    fn train_json(
        &self,
        model: &str,
        source: &str,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError>;
    fn train_table(
        &self,
        model: &str,
        source: &str,
        options: &LearnOptions,
    ) -> Result<DeepLearnOutput, SQLError>;
}
pub fn run_deep_learn_projection(
    models: &dyn ModelTraining,
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
