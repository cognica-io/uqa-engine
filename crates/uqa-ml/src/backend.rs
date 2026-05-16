//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use thiserror::Error;
use uqa_core::DocId;
use uqa_operators::ExecutionContext;

use crate::model::{predict_cpu, predict_feature_batch_cpu, DeepModel, PredictResult};
use crate::training::{deep_learn, DeepLearnOutput, LearnOptions, TrainingSet};

#[derive(Debug, Error)]
pub enum MLError {
    #[error("{0}")]
    InvalidModel(String),
    #[error("{0}")]
    InvalidTrainingSet(String),
    #[error("{0}")]
    Backend(String),
}

pub type MLResult<T> = Result<T, MLError>;

pub trait MLBackend {
    fn name(&self) -> &'static str;

    fn predict(&self, model: &DeepModel, ctx: &ExecutionContext) -> MLResult<PredictResult>;

    fn predict_features(
        &self,
        model: &DeepModel,
        examples: &[(DocId, Vec<f64>)],
    ) -> MLResult<PredictResult>;

    fn deep_learn(
        &self,
        training_set: &TrainingSet,
        options: &LearnOptions,
    ) -> MLResult<DeepLearnOutput>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CPUBackend;

impl MLBackend for CPUBackend {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn predict(&self, model: &DeepModel, ctx: &ExecutionContext) -> MLResult<PredictResult> {
        Ok(predict_cpu(model, ctx))
    }

    fn predict_features(
        &self,
        model: &DeepModel,
        examples: &[(DocId, Vec<f64>)],
    ) -> MLResult<PredictResult> {
        predict_feature_batch_cpu(model, examples)
    }

    fn deep_learn(
        &self,
        training_set: &TrainingSet,
        options: &LearnOptions,
    ) -> MLResult<DeepLearnOutput> {
        deep_learn(training_set, options)
    }
}
