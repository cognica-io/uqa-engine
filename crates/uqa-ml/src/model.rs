//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serializable deep-fusion model specs plus CPU inference helpers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uqa_core::{DocId, Value};
use uqa_operators::{base::Direction, ExecutionContext, Operator};

use crate::backend::{MLBackend, MLError, MLResult};
use crate::deep_fusion::{
    AggregationKind, DeepFusionOperator, Gating, GlobalPoolMethod, Layer, PoolMethod,
};

/// Output of [`DeepModel`] inference: `(doc_id, score)` pairs plus,
/// when the model ends in `Softmax`, per-doc class probability vectors.
pub type PredictResult = (Vec<(DocId, f64)>, BTreeMap<DocId, Vec<f64>>);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeepLayerSpec {
    /// Runtime feature-vector input. Used by trained models and batched
    /// feature inference.
    Input {
        dimensions: usize,
    },
    Embed {
        embedding: Vec<f64>,
    },
    Dense {
        weights: Vec<f64>,
        bias: Vec<f64>,
        output_channels: usize,
        input_channels: usize,
    },
    Flatten,
    GlobalPool {
        method: PoolMethodSpec,
    },
    Softmax,
    BatchNorm {
        epsilon: f64,
    },
    Dropout {
        p: f64,
    },
    Propagate {
        edge_label: String,
        aggregation: AggregationSpec,
        direction: DirectionSpec,
    },
    Conv {
        edge_label: String,
        hop_weights: Vec<f64>,
        direction: DirectionSpec,
    },
    Pool {
        edge_label: String,
        pool_size: usize,
        method: PoolKindSpec,
        direction: DirectionSpec,
    },
    Attention,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PoolMethodSpec {
    Avg,
    Max,
    AvgMax,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AggregationSpec {
    Mean,
    Sum,
    Max,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DirectionSpec {
    Out,
    In,
    Both,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PoolKindSpec {
    Avg,
    Max,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GatingSpec {
    #[default]
    None,
    ReLU,
    Swish,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeepModel {
    pub layers: Vec<DeepLayerSpec>,
    pub alpha: f64,
    pub gating: GatingSpec,
}

impl DeepModel {
    pub fn to_layers(&self) -> Vec<Layer> {
        self.layers.iter().map(layer_from_spec).collect()
    }

    pub fn gating_runtime(&self) -> Gating {
        match self.gating {
            GatingSpec::None => Gating::None,
            GatingSpec::ReLU => Gating::ReLU,
            GatingSpec::Swish => Gating::Swish,
        }
    }

    pub fn input_dimensions(&self) -> Option<usize> {
        match self.layers.first() {
            Some(DeepLayerSpec::Input { dimensions }) => Some(*dimensions),
            _ => None,
        }
    }

    /// Run CPU inference against an operator execution context.
    pub fn predict(&self, ctx: &ExecutionContext) -> PredictResult {
        predict_cpu(self, ctx)
    }

    /// Run inference through a specific backend.
    pub fn predict_with_backend<B: MLBackend>(
        &self,
        backend: &B,
        ctx: &ExecutionContext,
    ) -> MLResult<PredictResult> {
        backend.predict(self, ctx)
    }

    pub fn predict_features(&self, examples: &[(DocId, Vec<f64>)]) -> MLResult<PredictResult> {
        predict_feature_batch_cpu(self, examples)
    }

    pub fn predict_features_with_backend<B: MLBackend>(
        &self,
        backend: &B,
        examples: &[(DocId, Vec<f64>)],
    ) -> MLResult<PredictResult> {
        backend.predict_features(self, examples)
    }
}

pub(crate) fn predict_cpu(model: &DeepModel, ctx: &ExecutionContext) -> PredictResult {
    let layers = model.to_layers();
    if layers.is_empty() {
        return (Vec::new(), BTreeMap::new());
    }
    let op = DeepFusionOperator::new(layers, model.alpha, model.gating_runtime());
    posting_list_to_prediction(&op.execute(ctx))
}

pub(crate) fn predict_feature_batch_cpu(
    model: &DeepModel,
    examples: &[(DocId, Vec<f64>)],
) -> MLResult<PredictResult> {
    let layers = model.to_layers();
    if layers.is_empty() {
        return Ok((Vec::new(), BTreeMap::new()));
    }
    let Some(expected_dims) = model.input_dimensions() else {
        return Err(MLError::InvalidModel(
            "feature prediction requires a model whose first layer is Input".into(),
        ));
    };
    let op = DeepFusionOperator::new(layers, model.alpha, model.gating_runtime());
    let mut scores = Vec::with_capacity(examples.len());
    let mut probs = BTreeMap::new();
    for (doc_id, features) in examples {
        if features.len() != expected_dims {
            return Err(MLError::InvalidModel(format!(
                "feature vector for doc {doc_id} has dimension {}, expected {expected_dims}",
                features.len()
            )));
        }
        let sample_prediction =
            op.execute_features(*doc_id, features.clone(), &ExecutionContext::new());
        let (mut sample_scores, sample_probs) = posting_list_to_prediction(&sample_prediction);
        scores.append(&mut sample_scores);
        probs.extend(sample_probs);
    }
    scores.sort_by_key(|(doc_id, _)| *doc_id);
    Ok((scores, probs))
}

pub(crate) fn posting_list_to_prediction(pl: &uqa_core::PostingList) -> PredictResult {
    let mut scores: Vec<(DocId, f64)> = Vec::with_capacity(pl.len());
    let mut probs: BTreeMap<DocId, Vec<f64>> = BTreeMap::new();
    for entry in pl.entries() {
        scores.push((entry.doc_id, entry.payload.score));
        if let Some(Value::List(items)) = entry.payload.fields.get("class_probs") {
            let v: Vec<f64> = items
                .iter()
                .filter_map(|x| match x {
                    Value::Float(f) => Some(*f),
                    Value::Int(n) => Some(*n as f64),
                    _ => None,
                })
                .collect();
            probs.insert(entry.doc_id, v);
        }
    }
    (scores, probs)
}

fn layer_from_spec(spec: &DeepLayerSpec) -> Layer {
    match spec {
        DeepLayerSpec::Input { dimensions } => Layer::Input {
            dimensions: *dimensions,
        },
        DeepLayerSpec::Embed { embedding } => Layer::Embed(embedding.clone()),
        DeepLayerSpec::Dense {
            weights,
            bias,
            output_channels,
            input_channels,
        } => Layer::Dense {
            weights: weights.clone(),
            bias: bias.clone(),
            output_channels: *output_channels,
            input_channels: *input_channels,
        },
        DeepLayerSpec::Flatten => Layer::Flatten,
        DeepLayerSpec::GlobalPool { method } => Layer::GlobalPool(match method {
            PoolMethodSpec::Avg => GlobalPoolMethod::Avg,
            PoolMethodSpec::Max => GlobalPoolMethod::Max,
            PoolMethodSpec::AvgMax => GlobalPoolMethod::AvgMax,
        }),
        DeepLayerSpec::Softmax => Layer::Softmax,
        DeepLayerSpec::BatchNorm { epsilon } => Layer::BatchNorm { epsilon: *epsilon },
        DeepLayerSpec::Dropout { p } => Layer::Dropout { p: *p },
        DeepLayerSpec::Propagate {
            edge_label,
            aggregation,
            direction,
        } => Layer::Propagate {
            edge_label: edge_label.clone(),
            aggregation: match aggregation {
                AggregationSpec::Mean => AggregationKind::Mean,
                AggregationSpec::Sum => AggregationKind::Sum,
                AggregationSpec::Max => AggregationKind::Max,
            },
            direction: direction_runtime(*direction),
        },
        DeepLayerSpec::Conv {
            edge_label,
            hop_weights,
            direction,
        } => Layer::Conv {
            edge_label: edge_label.clone(),
            hop_weights: hop_weights.clone(),
            direction: direction_runtime(*direction),
        },
        DeepLayerSpec::Pool {
            edge_label,
            pool_size,
            method,
            direction,
        } => Layer::Pool {
            edge_label: edge_label.clone(),
            pool_size: *pool_size,
            method: match method {
                PoolKindSpec::Avg => PoolMethod::Avg,
                PoolKindSpec::Max => PoolMethod::Max,
            },
            direction: direction_runtime(*direction),
        },
        DeepLayerSpec::Attention => Layer::Attention,
    }
}

fn direction_runtime(dir: DirectionSpec) -> Direction {
    match dir {
        DirectionSpec::Out => Direction::Out,
        DirectionSpec::In => Direction::In,
        DirectionSpec::Both => Direction::Both,
    }
}
