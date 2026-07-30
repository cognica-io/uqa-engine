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

pub(crate) fn try_vec_with_capacity<T>(capacity: usize, context: &str) -> MLResult<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|error| {
        MLError::Backend(format!(
            "cannot allocate {capacity} elements for {context}: {error}"
        ))
    })?;
    Ok(values)
}

pub(crate) fn try_filled_vec<T: Clone>(length: usize, value: T, context: &str) -> MLResult<Vec<T>> {
    let mut values = try_vec_with_capacity(length, context)?;
    values.resize(length, value);
    Ok(values)
}

pub(crate) fn try_clone_slice<T: Clone>(values: &[T], context: &str) -> MLResult<Vec<T>> {
    let mut cloned = try_vec_with_capacity(values.len(), context)?;
    cloned.extend_from_slice(values);
    Ok(cloned)
}

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
        predict_cpu(model, ctx)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impossible_runtime_allocations_are_errors() {
        let error = try_filled_vec(usize::MAX, 0_u8, "test model channels")
            .expect_err("an impossible external dimension must not panic or abort");
        assert!(error.to_string().contains("cannot allocate"));
    }
}
