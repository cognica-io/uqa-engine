//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serializable deep-fusion model specs plus CPU inference helpers.

use std::collections::BTreeMap;

use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use uqa_core::{DocId, Value};
use uqa_operators::{base::Direction, ExecutionContext, Operator};

use crate::backend::{try_clone_slice, try_vec_with_capacity, MLBackend, MLError, MLResult};
use crate::deep_fusion::{
    AggregationKind, DeepFusionOperator, Gating, GlobalPoolMethod, Layer, PoolMethod,
};

/// Output of [`DeepModel`] inference: `(doc_id, score)` pairs plus,
/// when the model ends in `Softmax`, per-doc class probability vectors.
pub type PredictResult = (Vec<(DocId, f64)>, BTreeMap<DocId, Vec<f64>>);

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Serialize, PartialEq)]
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
    #[serde(rename = "cnn_1d")]
    CNN1D {
        weights: Vec<f64>,
        bias: Vec<f64>,
        output_channels: usize,
        input_channels: usize,
        kernel_size: usize,
        stride: usize,
        padding: usize,
    },
    #[serde(rename = "cnn_2d")]
    CNN2D {
        weights: Vec<f64>,
        bias: Vec<f64>,
        output_channels: usize,
        input_channels: usize,
        input_height: usize,
        input_width: usize,
        kernel_height: usize,
        kernel_width: usize,
        stride_height: usize,
        stride_width: usize,
        padding_height: usize,
        padding_width: usize,
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
    #[serde(rename = "rnn")]
    RNN {
        weights_input: Vec<f64>,
        weights_hidden: Vec<f64>,
        bias: Vec<f64>,
        hidden_channels: usize,
        input_channels: usize,
        return_sequences: bool,
    },
    #[serde(rename = "lstm")]
    LSTM {
        weights_input: Vec<f64>,
        weights_hidden: Vec<f64>,
        bias: Vec<f64>,
        hidden_channels: usize,
        input_channels: usize,
        return_sequences: bool,
    },
}

impl<'de> Deserialize<'de> for DeepLayerSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut fields = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        let kind = take_layer_field::<String, D::Error>(&mut fields, "layer", "kind")?;
        macro_rules! field {
            ($name:literal) => {
                take_layer_field::<_, D::Error>(&mut fields, &kind, $name)
            };
        }
        Ok(match kind.as_str() {
            "input" => Self::Input {
                dimensions: field!("dimensions")?,
            },
            "embed" => Self::Embed {
                embedding: field!("embedding")?,
            },
            "dense" => Self::Dense {
                weights: field!("weights")?,
                bias: field!("bias")?,
                output_channels: field!("output_channels")?,
                input_channels: field!("input_channels")?,
            },
            "flatten" => Self::Flatten,
            "global_pool" => Self::GlobalPool {
                method: field!("method")?,
            },
            "softmax" => Self::Softmax,
            "batch_norm" => Self::BatchNorm {
                epsilon: field!("epsilon")?,
            },
            "dropout" => Self::Dropout { p: field!("p")? },
            "cnn_1d" => Self::CNN1D {
                weights: field!("weights")?,
                bias: field!("bias")?,
                output_channels: field!("output_channels")?,
                input_channels: field!("input_channels")?,
                kernel_size: field!("kernel_size")?,
                stride: field!("stride")?,
                padding: field!("padding")?,
            },
            "cnn_2d" => Self::CNN2D {
                weights: field!("weights")?,
                bias: field!("bias")?,
                output_channels: field!("output_channels")?,
                input_channels: field!("input_channels")?,
                input_height: field!("input_height")?,
                input_width: field!("input_width")?,
                kernel_height: field!("kernel_height")?,
                kernel_width: field!("kernel_width")?,
                stride_height: field!("stride_height")?,
                stride_width: field!("stride_width")?,
                padding_height: field!("padding_height")?,
                padding_width: field!("padding_width")?,
            },
            "propagate" => Self::Propagate {
                edge_label: field!("edge_label")?,
                aggregation: field!("aggregation")?,
                direction: field!("direction")?,
            },
            "conv" => Self::Conv {
                edge_label: field!("edge_label")?,
                hop_weights: field!("hop_weights")?,
                direction: field!("direction")?,
            },
            "pool" => Self::Pool {
                edge_label: field!("edge_label")?,
                pool_size: field!("pool_size")?,
                method: field!("method")?,
                direction: field!("direction")?,
            },
            "attention" => Self::Attention,
            "rnn" => Self::RNN {
                weights_input: field!("weights_input")?,
                weights_hidden: field!("weights_hidden")?,
                bias: field!("bias")?,
                hidden_channels: field!("hidden_channels")?,
                input_channels: field!("input_channels")?,
                return_sequences: field!("return_sequences")?,
            },
            "lstm" => Self::LSTM {
                weights_input: field!("weights_input")?,
                weights_hidden: field!("weights_hidden")?,
                bias: field!("bias")?,
                hidden_channels: field!("hidden_channels")?,
                input_channels: field!("input_channels")?,
                return_sequences: field!("return_sequences")?,
            },
            other => {
                return Err(serde::de::Error::custom(format!(
                    "unknown deep layer kind `{other}`"
                )))
            }
        })
    }
}

fn take_layer_field<T, E>(
    fields: &mut BTreeMap<String, serde_json::Value>,
    kind: &str,
    name: &'static str,
) -> Result<T, E>
where
    T: DeserializeOwned,
    E: serde::de::Error,
{
    let value = fields
        .remove(name)
        .ok_or_else(|| E::custom(format!("{kind} layer is missing `{name}`")))?;
    serde_json::from_value(value)
        .map_err(|error| E::custom(format!("invalid `{name}` in {kind} layer: {error}")))
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
    /// Legacy Python catalogs omitted fusion tuning when the model only
    /// carried a layer graph. Zero preserves the historical neutral value.
    #[serde(default)]
    pub alpha: f64,
    #[serde(default)]
    pub gating: GatingSpec,
}

impl DeepModel {
    pub fn to_layers(&self) -> MLResult<Vec<Layer>> {
        let mut layers = try_vec_with_capacity(self.layers.len(), "deep model runtime layers")?;
        for spec in &self.layers {
            layers.push(layer_from_spec(spec)?);
        }
        Ok(layers)
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

    /// Validate layer ordering, tensor shapes, dimensions, and numeric parameters.
    pub fn validate(&self) -> MLResult<()> {
        DeepFusionOperator::new(self.to_layers()?, self.alpha, self.gating_runtime()).map(|_| ())
    }

    /// Run CPU inference against an operator execution context.
    pub fn predict(&self, ctx: &ExecutionContext) -> MLResult<PredictResult> {
        predict_cpu(self, ctx)
    }

    /// Run inference through a specific backend.
    pub fn predict_with_backend<B: MLBackend>(
        &self,
        backend: &B,
        ctx: &ExecutionContext,
    ) -> MLResult<PredictResult> {
        self.validate()?;
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
        self.validate()?;
        backend.predict_features(self, examples)
    }
}

pub(crate) fn predict_cpu(model: &DeepModel, ctx: &ExecutionContext) -> MLResult<PredictResult> {
    let layers = model.to_layers()?;
    if layers.is_empty() {
        return Err(MLError::InvalidModel(
            "deep model requires at least one layer".into(),
        ));
    }
    if model.input_dimensions().is_some() {
        return Err(MLError::InvalidModel(
            "Input-based models require predict_features rather than context-only predict".into(),
        ));
    }
    let op = DeepFusionOperator::new(layers, model.alpha, model.gating_runtime())?;
    let posting_list = op
        .execute(ctx)
        .map_err(|error| MLError::Backend(error.to_string()))?;
    posting_list_to_prediction(&posting_list)
}

pub(crate) fn predict_feature_batch_cpu(
    model: &DeepModel,
    examples: &[(DocId, Vec<f64>)],
) -> MLResult<PredictResult> {
    let layers = model.to_layers()?;
    if layers.is_empty() {
        return Err(MLError::InvalidModel(
            "deep model requires at least one layer".into(),
        ));
    }
    let Some(expected_dims) = model.input_dimensions() else {
        return Err(MLError::InvalidModel(
            "feature prediction requires a model whose first layer is Input".into(),
        ));
    };
    let op = DeepFusionOperator::new(layers, model.alpha, model.gating_runtime())?;
    let mut scores = try_vec_with_capacity(examples.len(), "feature prediction scores")?;
    let mut probs = BTreeMap::new();
    for (doc_id, features) in examples {
        if features.len() != expected_dims {
            return Err(MLError::InvalidModel(format!(
                "feature vector for doc {doc_id} has dimension {}, expected {expected_dims}",
                features.len()
            )));
        }
        let feature_copy = try_clone_slice(features, "feature prediction input")?;
        let sample_prediction = op
            .execute_features(*doc_id, feature_copy, &ExecutionContext::new())
            .map_err(|error| MLError::Backend(error.to_string()))?;
        let (mut sample_scores, sample_probs) = posting_list_to_prediction(&sample_prediction)?;
        scores.append(&mut sample_scores);
        probs.extend(sample_probs);
    }
    scores.sort_by_key(|(doc_id, _)| *doc_id);
    Ok((scores, probs))
}

pub(crate) fn posting_list_to_prediction(pl: &uqa_core::PostingList) -> MLResult<PredictResult> {
    let mut scores: Vec<(DocId, f64)> =
        try_vec_with_capacity(pl.len(), "posting-list prediction scores")?;
    let mut probs: BTreeMap<DocId, Vec<f64>> = BTreeMap::new();
    for entry in pl.entries() {
        if !entry.payload.score.is_finite() {
            return Err(MLError::Backend(format!(
                "prediction score for doc {} is not finite: {}",
                entry.doc_id, entry.payload.score
            )));
        }
        scores.push((entry.doc_id, entry.payload.score));
        if let Some(Value::List(items)) = entry.payload.fields.get("class_probs") {
            let v: Vec<f64> = items
                .iter()
                .enumerate()
                .map(|(index, value)| match value {
                    Value::Float(value) if value.is_finite() => Ok(*value),
                    other => Err(MLError::Backend(format!(
                        "class_probs[{index}] for doc {} is not a finite float: {other:?}",
                        entry.doc_id
                    ))),
                })
                .collect::<MLResult<_>>()?;
            probs.insert(entry.doc_id, v);
        }
    }
    Ok((scores, probs))
}

fn layer_from_spec(spec: &DeepLayerSpec) -> MLResult<Layer> {
    Ok(match spec {
        DeepLayerSpec::Input { dimensions } => Layer::Input {
            dimensions: *dimensions,
        },
        DeepLayerSpec::Embed { embedding } => {
            Layer::Embed(try_clone_slice(embedding, "embedding layer")?)
        }
        DeepLayerSpec::Dense {
            weights,
            bias,
            output_channels,
            input_channels,
        } => Layer::Dense {
            weights: try_clone_slice(weights, "dense weights")?,
            bias: try_clone_slice(bias, "dense bias")?,
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
        DeepLayerSpec::CNN1D { .. } => cnn_1d_from_spec(spec)?,
        DeepLayerSpec::CNN2D { .. } => cnn_2d_from_spec(spec)?,
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
            hop_weights: try_clone_slice(hop_weights, "graph convolution hop weights")?,
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
        DeepLayerSpec::RNN { .. } => recurrent_from_spec(spec, false)?,
        DeepLayerSpec::LSTM { .. } => recurrent_from_spec(spec, true)?,
    })
}

fn cnn_1d_from_spec(spec: &DeepLayerSpec) -> MLResult<Layer> {
    let DeepLayerSpec::CNN1D {
        weights,
        bias,
        output_channels,
        input_channels,
        kernel_size,
        stride,
        padding,
    } = spec
    else {
        return Err(MLError::InvalidModel(
            "internal CNN1D conversion mismatch".into(),
        ));
    };
    Ok(Layer::CNN1D {
        weights: try_clone_slice(weights, "CNN1D weights")?,
        bias: try_clone_slice(bias, "CNN1D bias")?,
        output_channels: *output_channels,
        input_channels: *input_channels,
        kernel_size: *kernel_size,
        stride: *stride,
        padding: *padding,
    })
}

fn cnn_2d_from_spec(spec: &DeepLayerSpec) -> MLResult<Layer> {
    let DeepLayerSpec::CNN2D {
        weights,
        bias,
        output_channels,
        input_channels,
        input_height,
        input_width,
        kernel_height,
        kernel_width,
        stride_height,
        stride_width,
        padding_height,
        padding_width,
    } = spec
    else {
        return Err(MLError::InvalidModel(
            "internal CNN2D conversion mismatch".into(),
        ));
    };
    Ok(Layer::CNN2D {
        weights: try_clone_slice(weights, "CNN2D weights")?,
        bias: try_clone_slice(bias, "CNN2D bias")?,
        output_channels: *output_channels,
        input_channels: *input_channels,
        input_height: *input_height,
        input_width: *input_width,
        kernel_height: *kernel_height,
        kernel_width: *kernel_width,
        stride_height: *stride_height,
        stride_width: *stride_width,
        padding_height: *padding_height,
        padding_width: *padding_width,
    })
}

fn recurrent_from_spec(spec: &DeepLayerSpec, lstm: bool) -> MLResult<Layer> {
    let (weights_input, weights_hidden, bias, hidden_channels, input_channels, return_sequences) =
        match spec {
            DeepLayerSpec::RNN {
                weights_input,
                weights_hidden,
                bias,
                hidden_channels,
                input_channels,
                return_sequences,
            }
            | DeepLayerSpec::LSTM {
                weights_input,
                weights_hidden,
                bias,
                hidden_channels,
                input_channels,
                return_sequences,
            } => (
                weights_input,
                weights_hidden,
                bias,
                *hidden_channels,
                *input_channels,
                *return_sequences,
            ),
            _ => {
                return Err(MLError::InvalidModel(
                    "internal recurrent conversion mismatch".into(),
                ));
            }
        };
    let prefix = if lstm { "LSTM" } else { "RNN" };
    let weights_input = try_clone_slice(weights_input, &format!("{prefix} input weights"))?;
    let weights_hidden = try_clone_slice(weights_hidden, &format!("{prefix} hidden weights"))?;
    let bias = try_clone_slice(bias, &format!("{prefix} bias"))?;
    Ok(if lstm {
        Layer::LSTM {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            return_sequences,
        }
    } else {
        Layer::RNN {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            return_sequences,
        }
    })
}

fn direction_runtime(dir: DirectionSpec) -> Direction {
    match dir {
        DirectionSpec::Out => Direction::Out,
        DirectionSpec::In => Direction::In,
        DirectionSpec::Both => Direction::Both,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{Payload, PostingEntry, PostingList};

    #[test]
    fn legacy_layer_only_models_receive_neutral_fusion_defaults() {
        let model: DeepModel = serde_json::from_str(r#"{"layers":[]}"#).unwrap();
        assert_eq!(model.alpha, 0.0);
        assert_eq!(model.gating, GatingSpec::None);
    }

    #[test]
    fn recurrent_acronyms_serialize_as_plain_names() {
        let rnn = DeepLayerSpec::RNN {
            weights_input: vec![1.0],
            weights_hidden: vec![0.0],
            bias: vec![0.0],
            hidden_channels: 1,
            input_channels: 1,
            return_sequences: true,
        };
        let lstm = DeepLayerSpec::LSTM {
            weights_input: vec![0.0; 4],
            weights_hidden: vec![0.0; 4],
            bias: vec![0.0; 4],
            hidden_channels: 1,
            input_channels: 1,
            return_sequences: false,
        };

        let rnn_json = serde_json::to_value(&rnn).unwrap();
        let lstm_json = serde_json::to_value(&lstm).unwrap();
        assert_eq!(rnn_json["kind"], "rnn");
        assert_eq!(lstm_json["kind"], "lstm");

        let cnn = DeepLayerSpec::CNN1D {
            weights: vec![1.0],
            bias: vec![0.0],
            output_channels: 1,
            input_channels: 1,
            kernel_size: 1,
            stride: 1,
            padding: 0,
        };
        let cnn_json = serde_json::to_value(&cnn).unwrap();
        assert_eq!(cnn_json["kind"], "cnn_1d");
    }

    #[test]
    fn internally_tagged_layers_with_float_fields_round_trip_through_json() {
        let model = DeepModel {
            layers: vec![DeepLayerSpec::Dense {
                weights: vec![1.0, 0.0],
                bias: vec![0.5],
                output_channels: 1,
                input_channels: 2,
            }],
            alpha: 0.25,
            gating: GatingSpec::None,
        };
        let json = serde_json::to_string(&model).unwrap();
        let decoded = serde_json::from_str::<DeepModel>(&json)
            .unwrap_or_else(|error| panic!("failed to decode {json}: {error}"));
        assert_eq!(decoded, model);
    }

    #[test]
    fn prediction_conversion_rejects_non_finite_scores() {
        let postings =
            PostingList::from_unsorted(vec![PostingEntry::new(7, Payload::with_score(f64::NAN))]);
        let error = posting_list_to_prediction(&postings)
            .expect_err("non-finite predictions must cross the API as errors");
        assert!(error.to_string().contains("not finite"));
    }
}
