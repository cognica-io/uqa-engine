//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ML model specs, analytical training, and inference backends.

pub mod backend;
pub mod deep_fusion;
pub mod model;
pub mod training;

#[cfg(feature = "mlx")]
pub mod mlx;

pub use backend::{CPUBackend, MLBackend, MLError, MLResult};
pub use deep_fusion::{
    AggregationKind as DeepAggKind, DeepFusionOperator, Gating, GlobalPoolMethod, Layer,
    PoolMethod as DeepPoolMethod,
};
pub use model::{
    AggregationSpec, DeepLayerSpec, DeepModel, DirectionSpec, GatingSpec, PoolKindSpec,
    PoolMethodSpec, PredictResult,
};
pub use training::{
    deep_learn, DeepLearnOutput, LearnOptions, TrainingExample, TrainingReport, TrainingSet,
};

#[cfg(feature = "mlx")]
pub use mlx::MLXBackend;
