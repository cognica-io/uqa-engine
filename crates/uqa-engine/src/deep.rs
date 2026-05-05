//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serializable deep-fusion model: layer specs + alpha + gating.
//!
//! `DeepModel` is the JSON-serializable surface persisted into the
//! catalog under the `_models` table. `to_layers` converts the spec
//! list back into runtime `Layer` values (no `Signal` arm — signals
//! reference live operators that aren't serializable). `predict` runs
//! a forward pass through the deep-fusion operator and returns either
//! a per-document scalar score or a per-document class-probability
//! vector when the final layer is `Softmax`.
//!
//! The compromise vs. the Python catalog: we do not serialize
//! `SignalLayer` (which references arbitrary operator trees). For
//! pure inference / classification flows the deep-fusion model is
//! complete — embed → ... → softmax — and that's what the SQL
//! `deep_predict` hook exercises.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Output of [`DeepModel::predict`]: `(doc_id, score)` pairs plus, when
/// the model ends in `Softmax`, the per-doc class probability vector.
pub type PredictResult = (Vec<(u64, f64)>, BTreeMap<u64, Vec<f64>>);
use uqa_core::Value;
use uqa_operators::{
    base::Direction, deep_fusion::AggregationKind, deep_fusion::PoolMethod, DeepFusionOperator,
    ExecutionContext, Gating, GlobalPoolMethod, Layer, Operator,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeepLayerSpec {
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GatingSpec {
    None,
    Relu,
    Swish,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
            GatingSpec::Relu => Gating::Relu,
            GatingSpec::Swish => Gating::Swish,
        }
    }

    /// Run a forward pass against `ctx` (with whatever document store /
    /// graph is wired up) and return the resulting `(doc_id, score)`
    /// pairs plus, when the model ends in `Softmax`, the per-doc class
    /// probability vector.
    pub fn predict(&self, ctx: &ExecutionContext) -> PredictResult {
        let layers = self.to_layers();
        if layers.is_empty() {
            return (Vec::new(), BTreeMap::new());
        }
        let op = DeepFusionOperator::new(layers, self.alpha, self.gating_runtime());
        let pl = op.execute(ctx);
        let mut scores: Vec<(u64, f64)> = Vec::with_capacity(pl.len());
        let mut probs: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
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
}

fn layer_from_spec(spec: &DeepLayerSpec) -> Layer {
    match spec {
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
