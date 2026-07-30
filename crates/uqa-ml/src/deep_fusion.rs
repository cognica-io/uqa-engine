//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-layer fusion operator (Section 7, Paper 4).
//!
//! Implements deep Bayesian fusion as a multi-layer network:
//!
//! ```text
//!     l^(k) = g( l^(k-1) + sum_j logit(P_j^(k)) )
//!     P_final = sigmoid(l^(K))
//! ```
//!
//! The internal channel map keys per-document feature vectors. Each
//! `Layer` variant updates that map, including signal, dense, convolutional,
//! recurrent, normalization, attention, and graph-aware propagation / pooling
//! layers.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{IndexStats, Payload, PostingEntry, PostingList, Value};
use uqa_scoring::prob::{log_odds_conjunction_weighted, logit, sigmoid, PROB_EPSILON};

use uqa_operators::{
    base::{Direction, OperatorResult},
    ExecutionContext, Operator,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

use crate::backend::{try_filled_vec, try_vec_with_capacity, MLError, MLResult};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Gating {
    #[default]
    None,
    Softplus,
    Sigmoid,
    ReLU,
    Swish,
    Gelu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalPoolMethod {
    Avg,
    Max,
    AvgMax,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregationKind {
    Mean,
    Sum,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolMethod {
    Avg,
    Max,
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone)]
pub enum Layer {
    /// Runtime-provided feature vector. This layer is a no-op during
    /// execution; it marks the expected input dimension for trained
    /// models that receive feature batches from an ML backend.
    Input { dimensions: usize },
    /// Run a list of `Operator` signals, fuse them via log-odds
    /// conjunction at the configured `alpha`, then add the resulting
    /// logit to channel 0 as a residual connection.
    Signal(Vec<Arc<dyn Operator>>),
    /// Initialize the channel map from a raw embedding vector. Element
    /// `i` becomes node `i+1` with a single-channel value.
    Embed(Vec<f64>),
    /// Fully connected: `out = W @ input + bias`, then gating.
    Dense {
        /// `output_channels x input_channels`, row-major.
        weights: Vec<f64>,
        bias: Vec<f64>,
        output_channels: usize,
        input_channels: usize,
    },
    /// Concatenate every node's channel vector into a single vector.
    Flatten,
    /// Reduce all spatial nodes to one vector.
    GlobalPool(GlobalPoolMethod),
    /// Numerically stable softmax per node.
    Softmax,
    /// Per-channel batch normalization across all nodes.
    BatchNorm { epsilon: f64 },
    /// Inference-mode dropout: scale every value by `1 - p`.
    Dropout { p: f64 },
    /// One-dimensional CNN over sorted sequence positions.
    ///
    /// Weights are row-major as `output_channels x kernel_size x input_channels`.
    CNN1D {
        weights: Vec<f64>,
        bias: Vec<f64>,
        output_channels: usize,
        input_channels: usize,
        kernel_size: usize,
        stride: usize,
        padding: usize,
    },
    /// Two-dimensional CNN over flattened `H x W x C` spatial positions.
    ///
    /// Weights are row-major as
    /// `output_channels x kernel_height x kernel_width x input_channels`.
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
    /// Propagate channel-0 scores through graph edges.
    ///
    /// `aggregation` averages / sums / maxes the in-bounds neighbor
    /// probabilities; the resulting logit is added as a residual on
    /// channel 0. Requires `ExecutionContext::graph`.
    Propagate {
        /// Edge label to follow. An empty string selects every edge label.
        edge_label: String,
        aggregation: AggregationKind,
        direction: Direction,
    },
    /// Weighted multi-hop graph convolution on channel 0.
    ///
    /// `hop_weights[0]` is the self weight, `hop_weights[i]` weights
    /// the average over the hop-`i` neighbor ring. Weights are
    /// L1-normalized; the result is converted back to logit and added
    /// as a residual.
    Conv {
        /// Edge label to follow. An empty string selects every edge label.
        edge_label: String,
        hop_weights: Vec<f64>,
        direction: Direction,
    },
    /// Spatial downsampling via greedy BFS partitioning.
    ///
    /// Groups `pool_size` neighboring nodes via BFS, aggregates their
    /// channel vectors element-wise (`PoolMethod::{Avg, Max}`), and
    /// keeps the smallest doc id as the representative.
    Pool {
        /// Edge label to follow. An empty string selects every edge label.
        edge_label: String,
        pool_size: usize,
        method: PoolMethod,
        direction: Direction,
    },
    /// Self-attention across the per-node channel vectors with
    /// `Q = K = V = X`, scaled-dot-product, no learned projections.
    Attention,
    /// Vanilla RNN over sorted sequence positions.
    ///
    /// Weights are row-major as `hidden_channels x input_channels` and
    /// `hidden_channels x hidden_channels`.
    RNN {
        weights_input: Vec<f64>,
        weights_hidden: Vec<f64>,
        bias: Vec<f64>,
        hidden_channels: usize,
        input_channels: usize,
        return_sequences: bool,
    },
    /// LSTM over sorted sequence positions.
    ///
    /// Gate order is input, forget, candidate, output. Both weight
    /// matrices are row-major with `4 * hidden_channels` rows.
    LSTM {
        weights_input: Vec<f64>,
        weights_hidden: Vec<f64>,
        bias: Vec<f64>,
        hidden_channels: usize,
        input_channels: usize,
        return_sequences: bool,
    },
}

pub struct DeepFusionOperator {
    layers: Vec<Layer>,
    alpha: f64,
    gating: Gating,
}

impl DeepFusionOperator {
    pub fn new(layers: Vec<Layer>, alpha: f64, gating: Gating) -> MLResult<Self> {
        validate_layers(&layers, alpha)?;
        Ok(Self {
            layers,
            alpha,
            gating,
        })
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    pub fn gating(&self) -> Gating {
        self.gating
    }

    fn validate_feature_input(&self, features: &[f64]) -> StorageBackendResult<()> {
        let Some(Layer::Input { dimensions }) = self.layers.first() else {
            return Err(runtime_model_error(
                "execute_features requires an Input-based deep-fusion model",
            ));
        };
        if features.len() != *dimensions {
            return Err(StorageBackendError::Other(format!(
                "deep-fusion feature vector has dimension {}, expected {dimensions}",
                features.len()
            )));
        }
        if let Some((index, value)) = features
            .iter()
            .enumerate()
            .find(|(_, value)| !value.is_finite())
        {
            return Err(StorageBackendError::Other(format!(
                "deep-fusion feature {index} must be finite, got {value}"
            )));
        }
        Ok(())
    }

    fn coverage_default(coverage: usize, total: usize) -> f64 {
        uqa_operators::hybrid::coverage_based_default(coverage, total, 0.01)
    }

    pub fn execute_features(
        &self,
        doc_id: u64,
        features: Vec<f64>,
        ctx: &ExecutionContext,
    ) -> OperatorResult {
        self.validate_feature_input(&features)?;
        let mut state = ForwardState {
            num_channels: features.len(),
            channel_map: BTreeMap::from([(doc_id, features)]),
            softmax_applied: false,
        };
        self.apply_layers(ctx, &mut state)?;
        build_result(
            &state.channel_map,
            state.num_channels,
            state.softmax_applied,
        )
    }

    fn apply_layers(
        &self,
        ctx: &ExecutionContext,
        state: &mut ForwardState,
    ) -> StorageBackendResult<()> {
        for layer in &self.layers {
            self.apply_layer(ctx, state, layer)?;
            validate_state(state)?;
        }
        Ok(())
    }

    fn apply_layer(
        &self,
        ctx: &ExecutionContext,
        state: &mut ForwardState,
        layer: &Layer,
    ) -> StorageBackendResult<()> {
        match layer {
            Layer::Input { dimensions } => {
                state.num_channels = *dimensions;
            }
            Layer::Embed(embedding) => apply_embed(embedding, state)?,
            Layer::Signal(signals) => {
                apply_signal(signals, ctx, self.alpha, self.gating, state)?;
            }
            Layer::Dense {
                weights,
                bias,
                output_channels,
                input_channels,
            } => apply_dense(
                weights,
                bias,
                *output_channels,
                *input_channels,
                self.gating,
                state,
            )?,
            Layer::Flatten => apply_flatten(state)?,
            Layer::GlobalPool(method) => apply_global_pool(*method, state)?,
            Layer::Softmax => apply_softmax(state)?,
            Layer::BatchNorm { epsilon } => apply_batch_norm(*epsilon, state)?,
            Layer::Dropout { p } => apply_dropout(*p, state),
            Layer::CNN1D { .. } => apply_cnn_1d_layer(layer, self.gating, state)?,
            Layer::CNN2D { .. } => apply_cnn_2d_layer(layer, self.gating, state)?,
            Layer::Propagate {
                edge_label,
                aggregation,
                direction,
            } => apply_propagate(
                edge_label,
                *aggregation,
                *direction,
                ctx,
                self.gating,
                state,
            )?,
            Layer::Conv {
                edge_label,
                hop_weights,
                direction,
            } => apply_conv(edge_label, hop_weights, *direction, ctx, self.gating, state)?,
            Layer::Pool {
                edge_label,
                pool_size,
                method,
                direction,
            } => apply_pool(edge_label, *pool_size, *method, *direction, ctx, state)?,
            Layer::Attention => apply_attention(state)?,
            Layer::RNN { .. } => apply_rnn_layer(layer, state)?,
            Layer::LSTM { .. } => apply_lstm_layer(layer, state)?,
        }
        Ok(())
    }
}

fn apply_cnn_1d_layer(
    layer: &Layer,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let Layer::CNN1D {
        weights,
        bias,
        output_channels,
        input_channels,
        kernel_size,
        stride,
        padding,
    } = layer
    else {
        return Err(runtime_model_error("internal CNN1D execution mismatch"));
    };
    apply_cnn_1d(
        Convolution1D {
            weights,
            bias,
            output_channels: *output_channels,
            input_channels: *input_channels,
            kernel_size: *kernel_size,
            stride: *stride,
            padding: *padding,
        },
        gating,
        state,
    )
}

fn apply_cnn_2d_layer(
    layer: &Layer,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let Layer::CNN2D {
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
    } = layer
    else {
        return Err(runtime_model_error("internal CNN2D execution mismatch"));
    };
    apply_cnn_2d(
        Convolution2D {
            weights,
            bias,
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
        },
        gating,
        state,
    )
}

fn apply_rnn_layer(layer: &Layer, state: &mut ForwardState) -> StorageBackendResult<()> {
    let Layer::RNN {
        weights_input,
        weights_hidden,
        bias,
        hidden_channels,
        input_channels,
        return_sequences,
    } = layer
    else {
        return Err(runtime_model_error("internal RNN execution mismatch"));
    };
    apply_rnn(
        Recurrent {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels: *hidden_channels,
            input_channels: *input_channels,
            return_sequences: *return_sequences,
        },
        state,
    )
}

fn apply_lstm_layer(layer: &Layer, state: &mut ForwardState) -> StorageBackendResult<()> {
    let Layer::LSTM {
        weights_input,
        weights_hidden,
        bias,
        hidden_channels,
        input_channels,
        return_sequences,
    } = layer
    else {
        return Err(runtime_model_error("internal LSTM execution mismatch"));
    };
    apply_lstm(
        LongShortTermMemory {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels: *hidden_channels,
            input_channels: *input_channels,
            return_sequences: *return_sequences,
        },
        state,
    )
}

impl Operator for DeepFusionOperator {
    fn execute(&self, ctx: &ExecutionContext) -> OperatorResult {
        if matches!(self.layers.first(), Some(Layer::Input { .. })) {
            return Err(runtime_model_error(
                "Input-based deep-fusion models require execute_features",
            ));
        }
        let mut state = ForwardState {
            channel_map: BTreeMap::new(),
            num_channels: 1,
            softmax_applied: false,
        };
        self.apply_layers(ctx, &mut state)?;
        build_result(
            &state.channel_map,
            state.num_channels,
            state.softmax_applied,
        )
    }

    fn cost_estimate(&self, stats: &IndexStats) -> f64 {
        let mut total = 0.0f64;
        for layer in &self.layers {
            match layer {
                Layer::Signal(signals) => {
                    for s in signals {
                        total += s.cost_estimate(stats);
                    }
                }
                Layer::Input { dimensions } => total += *dimensions as f64,
                Layer::Embed(emb) => total += emb.len() as f64,
                Layer::Dense {
                    output_channels,
                    input_channels,
                    ..
                } => total += (*output_channels as f64) * (*input_channels as f64),
                Layer::Flatten
                | Layer::GlobalPool(_)
                | Layer::Softmax
                | Layer::BatchNorm { .. }
                | Layer::Dropout { .. }
                | Layer::CNN1D { .. }
                | Layer::CNN2D { .. }
                | Layer::RNN { .. }
                | Layer::LSTM { .. }
                | Layer::Propagate { .. }
                | Layer::Conv { .. }
                | Layer::Pool { .. } => total += stats.total_docs as f64,
                Layer::Attention => {
                    let n = stats.total_docs as f64;
                    total += n * n;
                }
            }
        }
        total
    }
}

fn validate_layers(layers: &[Layer], alpha: f64) -> MLResult<()> {
    let Some(first) = layers.first() else {
        return Err(MLError::InvalidModel(
            "DeepFusionOperator requires at least one layer".into(),
        ));
    };
    if !matches!(
        first,
        Layer::Signal(_) | Layer::Embed(_) | Layer::Input { .. }
    ) {
        return Err(MLError::InvalidModel(
            "the first deep-fusion layer must be Signal, Embed, or Input".into(),
        ));
    }
    if !alpha.is_finite() {
        return Err(MLError::InvalidModel(format!(
            "deep-fusion alpha must be finite, got {alpha}"
        )));
    }

    for (index, layer) in layers.iter().enumerate() {
        validate_layer(index, layer)?;
    }
    Ok(())
}

fn validate_layer(index: usize, layer: &Layer) -> MLResult<()> {
    let context = format!("deep-fusion layer {index}");
    match layer {
        Layer::Input { dimensions } => validate_input(index, *dimensions, &context),
        Layer::Signal(signals) if signals.is_empty() => Err(MLError::InvalidModel(format!(
            "{context}: Signal requires at least one operator"
        ))),
        Layer::Embed(values) => validate_embedding(index, values, &context),
        Layer::Dense {
            weights,
            bias,
            output_channels,
            input_channels,
        } => validate_dense(weights, bias, *output_channels, *input_channels, &context),
        Layer::BatchNorm { epsilon } if !epsilon.is_finite() || *epsilon <= 0.0 => {
            Err(MLError::InvalidModel(format!(
                "{context}: epsilon must be finite and greater than zero"
            )))
        }
        Layer::Dropout { p } if !p.is_finite() || !(0.0..=1.0).contains(p) => {
            Err(MLError::InvalidModel(format!(
                "{context}: dropout probability must be finite and in [0, 1]"
            )))
        }
        Layer::CNN1D { .. } => validate_cnn_1d(layer, &context),
        Layer::CNN2D { .. } => validate_cnn_2d(layer, &context),
        Layer::Conv { hop_weights, .. } => validate_hop_weights(hop_weights, &context),
        Layer::Pool { pool_size, .. } => require_nonzero(*pool_size, &context, "pool_size"),
        Layer::RNN { .. } => validate_recurrent_spec(layer, 1, &context),
        Layer::LSTM { .. } => validate_recurrent_spec(layer, 4, &context),
        Layer::Signal(_)
        | Layer::BatchNorm { .. }
        | Layer::Dropout { .. }
        | Layer::Propagate { .. }
        | Layer::Flatten
        | Layer::GlobalPool(_)
        | Layer::Softmax
        | Layer::Attention => Ok(()),
    }
}

fn validate_input(index: usize, dimensions: usize, context: &str) -> MLResult<()> {
    if index != 0 {
        return Err(MLError::InvalidModel(format!(
            "{context}: Input is only valid as the first layer"
        )));
    }
    require_nonzero(dimensions, context, "dimensions")
}

fn validate_embedding(index: usize, values: &[f64], context: &str) -> MLResult<()> {
    if index != 0 {
        return Err(MLError::InvalidModel(format!(
            "{context}: Embed is only valid as the first layer"
        )));
    }
    if values.is_empty() {
        return Err(MLError::InvalidModel(format!(
            "{context}: embedding must not be empty"
        )));
    }
    require_finite(values, context, "embedding")
}

fn validate_dense(
    weights: &[f64],
    bias: &[f64],
    output_channels: usize,
    input_channels: usize,
    context: &str,
) -> MLResult<()> {
    require_nonzero(output_channels, context, "output_channels")?;
    require_nonzero(input_channels, context, "input_channels")?;
    let expected = checked_product(
        &[output_channels, input_channels],
        context,
        "dense weight count",
    )?;
    require_len(weights.len(), expected, context, "weights")?;
    require_len(bias.len(), output_channels, context, "bias")?;
    require_finite(weights, context, "weights")?;
    require_finite(bias, context, "bias")
}

fn validate_cnn_1d(layer: &Layer, context: &str) -> MLResult<()> {
    let Layer::CNN1D {
        weights,
        bias,
        output_channels,
        input_channels,
        kernel_size,
        stride,
        padding,
    } = layer
    else {
        return Err(MLError::InvalidModel(format!(
            "{context}: internal CNN1D validation mismatch"
        )));
    };
    for (name, value) in [
        ("output_channels", *output_channels),
        ("input_channels", *input_channels),
        ("kernel_size", *kernel_size),
        ("stride", *stride),
    ] {
        require_nonzero(value, context, name)?;
    }
    padding
        .checked_mul(2)
        .ok_or_else(|| MLError::InvalidModel(format!("{context}: padding is too large")))?;
    let expected = checked_product(
        &[*output_channels, *kernel_size, *input_channels],
        context,
        "CNN1D weight count",
    )?;
    validate_weights_and_bias(weights, bias, expected, *output_channels, context)
}

fn validate_cnn_2d(layer: &Layer, context: &str) -> MLResult<()> {
    let Layer::CNN2D {
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
    } = layer
    else {
        return Err(MLError::InvalidModel(format!(
            "{context}: internal CNN2D validation mismatch"
        )));
    };
    for (name, value) in [
        ("output_channels", *output_channels),
        ("input_channels", *input_channels),
        ("input_height", *input_height),
        ("input_width", *input_width),
        ("kernel_height", *kernel_height),
        ("kernel_width", *kernel_width),
        ("stride_height", *stride_height),
        ("stride_width", *stride_width),
    ] {
        require_nonzero(value, context, name)?;
    }
    padding_height
        .checked_mul(2)
        .ok_or_else(|| MLError::InvalidModel(format!("{context}: padding_height is too large")))?;
    padding_width
        .checked_mul(2)
        .ok_or_else(|| MLError::InvalidModel(format!("{context}: padding_width is too large")))?;
    checked_product(
        &[*input_height, *input_width, *input_channels],
        context,
        "CNN2D input size",
    )?;
    let expected = checked_product(
        &[
            *output_channels,
            *kernel_height,
            *kernel_width,
            *input_channels,
        ],
        context,
        "CNN2D weight count",
    )?;
    validate_weights_and_bias(weights, bias, expected, *output_channels, context)
}

fn validate_weights_and_bias(
    weights: &[f64],
    bias: &[f64],
    expected_weights: usize,
    output_channels: usize,
    context: &str,
) -> MLResult<()> {
    require_len(weights.len(), expected_weights, context, "weights")?;
    require_len(bias.len(), output_channels, context, "bias")?;
    require_finite(weights, context, "weights")?;
    require_finite(bias, context, "bias")
}

fn validate_hop_weights(hop_weights: &[f64], context: &str) -> MLResult<()> {
    if hop_weights.is_empty() {
        return Err(MLError::InvalidModel(format!(
            "{context}: graph convolution requires at least one hop weight"
        )));
    }
    require_finite(hop_weights, context, "hop_weights")?;
    let total_weight: f64 = hop_weights.iter().sum();
    if hop_weights.iter().any(|weight| *weight < 0.0)
        || !total_weight.is_finite()
        || total_weight <= 0.0
    {
        return Err(MLError::InvalidModel(format!(
            "{context}: hop_weights must be non-negative with a finite positive sum"
        )));
    }
    Ok(())
}

fn validate_recurrent_spec(layer: &Layer, gates: usize, context: &str) -> MLResult<()> {
    let (weights_input, weights_hidden, bias, hidden_channels, input_channels) = match layer {
        Layer::RNN {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            ..
        }
        | Layer::LSTM {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            ..
        } => (
            weights_input,
            weights_hidden,
            bias,
            *hidden_channels,
            *input_channels,
        ),
        _ => {
            return Err(MLError::InvalidModel(format!(
                "{context}: internal recurrent validation mismatch"
            )));
        }
    };
    validate_recurrent_layer(
        weights_input,
        weights_hidden,
        bias,
        hidden_channels,
        input_channels,
        gates,
        context,
    )
}

fn validate_recurrent_layer(
    weights_input: &[f64],
    weights_hidden: &[f64],
    bias: &[f64],
    hidden_channels: usize,
    input_channels: usize,
    gates: usize,
    context: &str,
) -> MLResult<()> {
    require_nonzero(hidden_channels, context, "hidden_channels")?;
    require_nonzero(input_channels, context, "input_channels")?;
    let gate_channels = checked_product(&[gates, hidden_channels], context, "gate channels")?;
    let input_count = checked_product(
        &[gate_channels, input_channels],
        context,
        "input weight count",
    )?;
    let hidden_count = checked_product(
        &[gate_channels, hidden_channels],
        context,
        "hidden weight count",
    )?;
    require_len(weights_input.len(), input_count, context, "weights_input")?;
    require_len(
        weights_hidden.len(),
        hidden_count,
        context,
        "weights_hidden",
    )?;
    require_len(bias.len(), gate_channels, context, "bias")?;
    require_finite(weights_input, context, "weights_input")?;
    require_finite(weights_hidden, context, "weights_hidden")?;
    require_finite(bias, context, "bias")
}

fn require_nonzero(value: usize, context: &str, field: &str) -> MLResult<()> {
    if value == 0 {
        Err(MLError::InvalidModel(format!(
            "{context}: {field} must be greater than zero"
        )))
    } else {
        Ok(())
    }
}

fn require_len(actual: usize, expected: usize, context: &str, field: &str) -> MLResult<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(MLError::InvalidModel(format!(
            "{context}: {field} has length {actual}, expected {expected}"
        )))
    }
}

fn require_finite(values: &[f64], context: &str, field: &str) -> MLResult<()> {
    if let Some((index, value)) = values
        .iter()
        .enumerate()
        .find(|(_, value)| !value.is_finite())
    {
        Err(MLError::InvalidModel(format!(
            "{context}: {field}[{index}] must be finite, got {value}"
        )))
    } else {
        Ok(())
    }
}

fn checked_product(values: &[usize], context: &str, description: &str) -> MLResult<usize> {
    values.iter().try_fold(1usize, |product, value| {
        product.checked_mul(*value).ok_or_else(|| {
            MLError::InvalidModel(format!("{context}: {description} overflows usize"))
        })
    })
}

fn validate_state(state: &ForwardState) -> StorageBackendResult<()> {
    if state.num_channels == 0 && !state.channel_map.is_empty() {
        return Err(StorageBackendError::Other(
            "deep-fusion layer produced zero channels".into(),
        ));
    }
    for (doc_id, values) in &state.channel_map {
        if values.len() != state.num_channels {
            return Err(StorageBackendError::Other(format!(
                "deep-fusion doc {doc_id} has {} channels, expected {}",
                values.len(),
                state.num_channels
            )));
        }
        if let Some((index, value)) = values
            .iter()
            .enumerate()
            .find(|(_, value)| !value.is_finite())
        {
            return Err(StorageBackendError::Other(format!(
                "deep-fusion doc {doc_id} channel {index} is non-finite: {value}"
            )));
        }
    }
    Ok(())
}

struct ForwardState {
    channel_map: BTreeMap<u64, Vec<f64>>,
    num_channels: usize,
    softmax_applied: bool,
}

#[derive(Clone, Copy)]
struct Convolution1D<'a> {
    weights: &'a [f64],
    bias: &'a [f64],
    output_channels: usize,
    input_channels: usize,
    kernel_size: usize,
    stride: usize,
    padding: usize,
}

#[derive(Clone, Copy)]
struct Convolution2D<'a> {
    weights: &'a [f64],
    bias: &'a [f64],
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
}

#[derive(Clone, Copy)]
struct Recurrent<'a> {
    weights_input: &'a [f64],
    weights_hidden: &'a [f64],
    bias: &'a [f64],
    hidden_channels: usize,
    input_channels: usize,
    return_sequences: bool,
}

#[derive(Clone, Copy)]
struct LongShortTermMemory<'a> {
    weights_input: &'a [f64],
    weights_hidden: &'a [f64],
    bias: &'a [f64],
    hidden_channels: usize,
    input_channels: usize,
    return_sequences: bool,
}

fn apply_embed(embedding: &[f64], state: &mut ForwardState) -> StorageBackendResult<()> {
    for (i, val) in embedding.iter().enumerate() {
        let index = u64::try_from(i)
            .map_err(|_| runtime_model_error("embedding index exceeds the u64 range"))?;
        let doc_id = index
            .checked_add(1)
            .ok_or_else(|| runtime_model_error("embedding index exceeds the document-ID range"))?;
        state.channel_map.insert(doc_id, vec![*val]);
    }
    state.num_channels = 1;
    state.softmax_applied = false;
    Ok(())
}

fn apply_signal(
    signals: &[Arc<dyn Operator>],
    ctx: &ExecutionContext,
    alpha: f64,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let mut posting_lists = runtime_vec_with_capacity(signals.len(), "deep-fusion signals")?;
    for signal in signals {
        posting_lists.push(signal.execute(ctx)?);
    }
    let mut score_maps = runtime_vec_with_capacity(signals.len(), "deep-fusion score maps")?;
    let mut all_doc_ids: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    for pl in &posting_lists {
        let mut smap: BTreeMap<u64, f64> = BTreeMap::new();
        for entry in pl.entries() {
            if !entry.payload.score.is_finite() || !(0.0..=1.0).contains(&entry.payload.score) {
                return Err(runtime_model_error(format!(
                    "deep-fusion signal score for doc {} must be a finite probability in [0, 1], got {}",
                    entry.doc_id, entry.payload.score
                )));
            }
            smap.insert(entry.doc_id, entry.payload.score);
            all_doc_ids.insert(entry.doc_id);
        }
        score_maps.push(smap);
    }
    if all_doc_ids.is_empty() {
        return Ok(());
    }
    let total = all_doc_ids.len();
    let mut defaults = runtime_vec_with_capacity(score_maps.len(), "signal defaults")?;
    defaults.extend(
        score_maps
            .iter()
            .map(|map| DeepFusionOperator::coverage_default(map.len(), total)),
    );
    for doc_id in &all_doc_ids {
        let mut probs = runtime_vec_with_capacity(score_maps.len(), "signal probabilities")?;
        probs.extend(
            score_maps
                .iter()
                .enumerate()
                .map(|(i, map)| map.get(doc_id).copied().unwrap_or(defaults[i])),
        );
        let fused = if probs.len() == 1 {
            probs[0]
        } else {
            let n = probs.len();
            let weights = runtime_filled_vec(
                n,
                1.0 / usize_to_f64_exact(n, "signal count")?,
                "deep-fusion signal weights",
            )?;
            log_odds_conjunction_weighted(&probs, &weights, alpha)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?
        };
        let layer_logit = apply_gating(safe_logit(fused), gating);
        let n = state.num_channels;
        if !state.channel_map.contains_key(doc_id) {
            let channels = runtime_filled_vec(n, 0.0, "deep-fusion signal channels")?;
            state.channel_map.insert(*doc_id, channels);
        }
        let entry = state.channel_map.get_mut(doc_id).ok_or_else(|| {
            runtime_model_error(format!("missing signal output for doc {doc_id}"))
        })?;
        entry[0] += layer_logit;
    }
    Ok(())
}

fn apply_dense(
    weights: &[f64],
    bias: &[f64],
    output_channels: usize,
    input_channels: usize,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let doc_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    for did in doc_ids {
        let input = state
            .channel_map
            .get(&did)
            .ok_or_else(|| runtime_model_error(format!("missing dense input for doc {did}")))?;
        if input.len() != input_channels {
            return Err(runtime_model_error(format!(
                "dense input for doc {did} has {} channels, expected {input_channels}",
                input.len()
            )));
        }
        let mut out = runtime_filled_vec(output_channels, 0.0f64, "dense output channels")?;
        for o in 0..output_channels {
            let mut acc = bias[o];
            for i in 0..input_channels {
                acc += weights[o * input_channels + i] * input[i];
            }
            out[o] = apply_gating(acc, gating);
        }
        state.channel_map.insert(did, out);
    }
    state.num_channels = output_channels;
    state.softmax_applied = false;
    Ok(())
}

fn apply_flatten(state: &mut ForwardState) -> StorageBackendResult<()> {
    let sorted_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if sorted_ids.is_empty() {
        return Ok(());
    }
    let flat_len = sorted_ids.iter().try_fold(0usize, |length, doc_id| {
        length
            .checked_add(state.channel_map[doc_id].len())
            .ok_or_else(|| runtime_model_error("flattened channel count overflows usize"))
    })?;
    let mut flat = runtime_vec_with_capacity(flat_len, "flattened channels")?;
    for did in &sorted_ids {
        if let Some(v) = state.channel_map.get(did) {
            flat.extend_from_slice(v);
        }
    }
    let new_n = flat.len();
    let rep = sorted_ids[0];
    state.channel_map.clear();
    state.channel_map.insert(rep, flat);
    state.num_channels = new_n;
    Ok(())
}

fn apply_global_pool(
    method: GlobalPoolMethod,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let sorted_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if sorted_ids.is_empty() {
        return Ok(());
    }
    let n_dims = state.channel_map[&sorted_ids[0]].len();
    let mut sums = runtime_filled_vec(n_dims, 0.0f64, "global-pool sums")?;
    let mut maxes = runtime_filled_vec(n_dims, f64::NEG_INFINITY, "global-pool maxima")?;
    for did in &sorted_ids {
        let v = &state.channel_map[did];
        if v.len() != n_dims {
            return Err(runtime_model_error(format!(
                "global-pool input for doc {did} has {} channels, expected {n_dims}",
                v.len()
            )));
        }
        for i in 0..n_dims {
            let x = v[i];
            sums[i] += x;
            if x > maxes[i] {
                maxes[i] = x;
            }
        }
    }
    let count = usize_to_f64_exact(sorted_ids.len(), "global-pool row count")?;
    let pooled: Vec<f64> = match method {
        GlobalPoolMethod::Avg => {
            let mut averages = runtime_vec_with_capacity(n_dims, "global-pool averages")?;
            averages.extend(sums.iter().map(|s| s / count));
            averages
        }
        GlobalPoolMethod::Max => crate::backend::try_clone_slice(&maxes, "global-pool output")
            .map_err(|error| runtime_model_error(error.to_string()))?,
        GlobalPoolMethod::AvgMax => {
            let capacity = n_dims
                .checked_mul(2)
                .ok_or_else(|| runtime_model_error("AvgMax output width overflows usize"))?;
            let mut combined = runtime_vec_with_capacity(capacity, "AvgMax output channels")?;
            combined.extend(sums.iter().map(|s| s / count));
            combined.extend(maxes.iter().copied());
            combined
        }
    };
    let new_n = pooled.len();
    let rep = sorted_ids[0];
    state.channel_map.clear();
    state.channel_map.insert(rep, pooled);
    state.num_channels = new_n;
    state.softmax_applied = false;
    Ok(())
}

fn apply_softmax(state: &mut ForwardState) -> StorageBackendResult<()> {
    for vec_ref in state.channel_map.values_mut() {
        if vec_ref.is_empty() {
            return Err(runtime_model_error("softmax input has zero channels"));
        }
        let max = vec_ref.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut exps = runtime_vec_with_capacity(vec_ref.len(), "softmax exponentials")?;
        exps.extend(vec_ref.iter().map(|x| (x - max).exp()));
        let sum: f64 = exps.iter().sum();
        if sum > 0.0 {
            for x in &mut exps {
                *x /= sum;
            }
        } else {
            let n = usize_to_f64_exact(exps.len(), "softmax channel count")?;
            for x in &mut exps {
                *x = 1.0 / n;
            }
        }
        *vec_ref = exps;
    }
    state.softmax_applied = true;
    Ok(())
}

fn apply_batch_norm(epsilon: f64, state: &mut ForwardState) -> StorageBackendResult<()> {
    if state.channel_map.len() < 2 {
        return Ok(());
    }
    let dim = state.channel_map.values().next().map_or(0, Vec::len);
    if dim == 0 {
        return Ok(());
    }
    let mut means = runtime_filled_vec(dim, 0.0f64, "batch-normalization means")?;
    for v in state.channel_map.values() {
        for i in 0..dim {
            means[i] += v[i];
        }
    }
    let n = usize_to_f64_exact(state.channel_map.len(), "batch-normalization row count")?;
    for m in &mut means {
        *m /= n;
    }
    let mut vars = runtime_filled_vec(dim, 0.0f64, "batch-normalization variances")?;
    for v in state.channel_map.values() {
        for i in 0..dim {
            let d = v[i] - means[i];
            vars[i] += d * d;
        }
    }
    for v in &mut vars {
        *v /= n;
    }
    for v in state.channel_map.values_mut() {
        for i in 0..dim {
            v[i] = (v[i] - means[i]) / (vars[i] + epsilon).sqrt();
        }
    }
    Ok(())
}

fn apply_dropout(p: f64, state: &mut ForwardState) {
    let scale = 1.0 - p;
    for v in state.channel_map.values_mut() {
        for x in v.iter_mut() {
            *x *= scale;
        }
    }
}

fn apply_cnn_1d(
    params: Convolution1D<'_>,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return Ok(());
    }
    for doc_id in &input_ids {
        let channels = state.channel_map[doc_id].len();
        if channels != params.input_channels {
            return Err(runtime_model_error(format!(
                "CNN1D input for doc {doc_id} has {channels} channels, expected {}",
                params.input_channels
            )));
        }
    }
    let double_padding = params
        .padding
        .checked_mul(2)
        .ok_or_else(|| runtime_model_error("CNN1D padding overflows usize"))?;
    let padded_len = input_ids
        .len()
        .checked_add(double_padding)
        .ok_or_else(|| runtime_model_error("CNN1D padded input length overflows usize"))?;
    if padded_len < params.kernel_size {
        state.channel_map.clear();
        state.num_channels = params.output_channels;
        state.softmax_applied = false;
        return Ok(());
    }
    let output_len = (padded_len - params.kernel_size) / params.stride + 1;
    let mut next_synthetic_id = input_ids.last().and_then(|doc_id| doc_id.checked_add(1));
    let mut output = BTreeMap::new();
    for out_pos in 0..output_len {
        let mut row = runtime_filled_vec(params.output_channels, 0.0f64, "CNN1D output channels")?;
        for (out_ch, slot) in row.iter_mut().enumerate() {
            let mut acc = params.bias[out_ch];
            for kernel_pos in 0..params.kernel_size {
                let raw_pos = out_pos
                    .checked_mul(params.stride)
                    .and_then(|position| position.checked_add(kernel_pos))
                    .ok_or_else(|| runtime_model_error("CNN1D window position overflows usize"))?;
                if raw_pos < params.padding {
                    continue;
                }
                let input_pos = raw_pos - params.padding;
                if input_pos >= input_ids.len() {
                    continue;
                }
                let input_vec = &state.channel_map[&input_ids[input_pos]];
                for (in_ch, input_value) in input_vec.iter().enumerate() {
                    let weight_index =
                        (out_ch * params.kernel_size + kernel_pos) * params.input_channels + in_ch;
                    acc += params.weights[weight_index] * input_value;
                }
            }
            *slot = apply_gating(acc, gating);
        }
        let doc_id = if let Some(doc_id) = input_ids.get(out_pos).copied() {
            doc_id
        } else {
            let doc_id = next_synthetic_id
                .ok_or_else(|| runtime_model_error("CNN1D output document IDs exceed u64"))?;
            next_synthetic_id = doc_id.checked_add(1);
            doc_id
        };
        output.insert(doc_id, row);
    }
    state.channel_map = output;
    state.num_channels = params.output_channels;
    state.softmax_applied = false;
    Ok(())
}

fn apply_cnn_2d(
    params: Convolution2D<'_>,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return Ok(());
    }
    let expected_input = params
        .input_height
        .checked_mul(params.input_width)
        .and_then(|size| size.checked_mul(params.input_channels))
        .ok_or_else(|| runtime_model_error("CNN2D input size overflows usize"))?;
    let mut flat_input = runtime_vec_with_capacity(expected_input, "CNN2D flattened input")?;
    if input_ids.len() == 1 {
        flat_input.extend_from_slice(&state.channel_map[&input_ids[0]]);
    } else {
        for doc_id in &input_ids {
            flat_input.extend_from_slice(&state.channel_map[doc_id]);
        }
    }

    if flat_input.len() != expected_input {
        return Err(runtime_model_error(format!(
            "CNN2D input has {} scalar values, expected {expected_input}",
            flat_input.len()
        )));
    }

    let padded_height = params
        .padding_height
        .checked_mul(2)
        .and_then(|padding| params.input_height.checked_add(padding))
        .ok_or_else(|| runtime_model_error("CNN2D padded height overflows usize"))?;
    let padded_width = params
        .padding_width
        .checked_mul(2)
        .and_then(|padding| params.input_width.checked_add(padding))
        .ok_or_else(|| runtime_model_error("CNN2D padded width overflows usize"))?;
    if padded_height < params.kernel_height || padded_width < params.kernel_width {
        state.channel_map.clear();
        state.num_channels = params.output_channels;
        state.softmax_applied = false;
        return Ok(());
    }
    let output_height = (padded_height - params.kernel_height) / params.stride_height + 1;
    let output_width = (padded_width - params.kernel_width) / params.stride_width + 1;
    let mut next_synthetic_id = input_ids.last().and_then(|doc_id| doc_id.checked_add(1));
    let mut output = BTreeMap::new();

    for out_row in 0..output_height {
        for out_col in 0..output_width {
            let mut row =
                runtime_filled_vec(params.output_channels, 0.0f64, "CNN2D output channels")?;
            for (out_ch, slot) in row.iter_mut().enumerate() {
                *slot = apply_gating(
                    cnn_2d_cell(&params, &flat_input, out_row, out_col, out_ch)?,
                    gating,
                );
            }
            let output_index = out_row
                .checked_mul(output_width)
                .and_then(|index| index.checked_add(out_col))
                .ok_or_else(|| runtime_model_error("CNN2D output index overflows usize"))?;
            let doc_id = if let Some(doc_id) = input_ids.get(output_index).copied() {
                doc_id
            } else {
                let doc_id = next_synthetic_id
                    .ok_or_else(|| runtime_model_error("CNN2D output document IDs exceed u64"))?;
                next_synthetic_id = doc_id.checked_add(1);
                doc_id
            };
            output.insert(doc_id, row);
        }
    }
    state.channel_map = output;
    state.num_channels = params.output_channels;
    state.softmax_applied = false;
    Ok(())
}

fn cnn_2d_cell(
    params: &Convolution2D<'_>,
    flat_input: &[f64],
    out_row: usize,
    out_col: usize,
    out_ch: usize,
) -> StorageBackendResult<f64> {
    let mut acc = params.bias[out_ch];
    for kernel_row in 0..params.kernel_height {
        let raw_row = out_row
            .checked_mul(params.stride_height)
            .and_then(|row| row.checked_add(kernel_row))
            .ok_or_else(|| runtime_model_error("CNN2D row position overflows usize"))?;
        if raw_row < params.padding_height {
            continue;
        }
        let input_row = raw_row - params.padding_height;
        if input_row >= params.input_height {
            continue;
        }
        for kernel_col in 0..params.kernel_width {
            let raw_col = out_col
                .checked_mul(params.stride_width)
                .and_then(|column| column.checked_add(kernel_col))
                .ok_or_else(|| runtime_model_error("CNN2D column position overflows usize"))?;
            if raw_col < params.padding_width {
                continue;
            }
            let input_col = raw_col - params.padding_width;
            if input_col >= params.input_width {
                continue;
            }
            for in_ch in 0..params.input_channels {
                let input_index =
                    ((input_row * params.input_width + input_col) * params.input_channels) + in_ch;
                let weight_index = (((out_ch * params.kernel_height + kernel_row)
                    * params.kernel_width
                    + kernel_col)
                    * params.input_channels)
                    + in_ch;
                acc += params.weights[weight_index] * flat_input[input_index];
            }
        }
    }
    Ok(acc)
}

fn apply_rnn(params: Recurrent<'_>, state: &mut ForwardState) -> StorageBackendResult<()> {
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return Ok(());
    }
    let mut hidden = runtime_filled_vec(params.hidden_channels, 0.0f64, "RNN hidden state")?;
    let mut output = BTreeMap::new();
    for doc_id in &input_ids {
        let input = &state.channel_map[doc_id];
        if input.len() != params.input_channels {
            return Err(runtime_model_error(format!(
                "RNN input for doc {doc_id} has {} channels, expected {}",
                input.len(),
                params.input_channels
            )));
        }
        let mut next_hidden =
            runtime_filled_vec(params.hidden_channels, 0.0f64, "RNN next hidden state")?;
        for (out_ch, slot) in next_hidden.iter_mut().enumerate() {
            let mut acc = params.bias[out_ch];
            for (in_ch, input_value) in input.iter().enumerate() {
                acc += params.weights_input[out_ch * params.input_channels + in_ch] * input_value;
            }
            for (hidden_ch, hidden_value) in hidden.iter().enumerate() {
                acc += params.weights_hidden[out_ch * params.hidden_channels + hidden_ch]
                    * hidden_value;
            }
            *slot = acc.tanh();
        }
        hidden = next_hidden;
        if params.return_sequences {
            let output_hidden = crate::backend::try_clone_slice(&hidden, "RNN output state")
                .map_err(|error| runtime_model_error(error.to_string()))?;
            output.insert(*doc_id, output_hidden);
        }
    }
    if !params.return_sequences {
        let last_doc_id = input_ids
            .last()
            .copied()
            .ok_or_else(|| runtime_model_error("RNN input unexpectedly became empty"))?;
        output.insert(last_doc_id, hidden);
    }
    state.channel_map = output;
    state.num_channels = params.hidden_channels;
    state.softmax_applied = false;
    Ok(())
}

fn apply_lstm(
    params: LongShortTermMemory<'_>,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    let gate_channels = params
        .hidden_channels
        .checked_mul(4)
        .ok_or_else(|| runtime_model_error("LSTM gate channel count overflows usize"))?;
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return Ok(());
    }
    let mut hidden = runtime_filled_vec(params.hidden_channels, 0.0f64, "LSTM hidden state")?;
    let mut cell = runtime_filled_vec(params.hidden_channels, 0.0f64, "LSTM cell state")?;
    let mut output = BTreeMap::new();
    for doc_id in &input_ids {
        let input = &state.channel_map[doc_id];
        if input.len() != params.input_channels {
            return Err(runtime_model_error(format!(
                "LSTM input for doc {doc_id} has {} channels, expected {}",
                input.len(),
                params.input_channels
            )));
        }
        let mut gates = runtime_filled_vec(gate_channels, 0.0f64, "LSTM gate channels")?;
        for (gate_ch, gate_slot) in gates.iter_mut().enumerate() {
            let mut acc = params.bias[gate_ch];
            for (in_ch, input_value) in input.iter().enumerate() {
                acc += params.weights_input[gate_ch * params.input_channels + in_ch] * input_value;
            }
            for (hidden_ch, hidden_value) in hidden.iter().enumerate() {
                acc += params.weights_hidden[gate_ch * params.hidden_channels + hidden_ch]
                    * hidden_value;
            }
            *gate_slot = acc;
        }

        let mut next_hidden =
            runtime_filled_vec(params.hidden_channels, 0.0f64, "LSTM next hidden state")?;
        let mut next_cell =
            runtime_filled_vec(params.hidden_channels, 0.0f64, "LSTM next cell state")?;
        for ch in 0..params.hidden_channels {
            let input_gate = sigmoid(gates[ch]);
            let forget_gate = sigmoid(gates[params.hidden_channels + ch]);
            let candidate = gates[2 * params.hidden_channels + ch].tanh();
            let output_gate = sigmoid(gates[3 * params.hidden_channels + ch]);
            next_cell[ch] = forget_gate * cell[ch] + input_gate * candidate;
            next_hidden[ch] = output_gate * next_cell[ch].tanh();
        }
        hidden = next_hidden;
        cell = next_cell;
        if params.return_sequences {
            let output_hidden = crate::backend::try_clone_slice(&hidden, "LSTM output state")
                .map_err(|error| runtime_model_error(error.to_string()))?;
            output.insert(*doc_id, output_hidden);
        }
    }
    if !params.return_sequences {
        let last_doc_id = input_ids
            .last()
            .copied()
            .ok_or_else(|| runtime_model_error("LSTM input unexpectedly became empty"))?;
        output.insert(last_doc_id, hidden);
    }
    state.channel_map = output;
    state.num_channels = params.hidden_channels;
    state.softmax_applied = false;
    Ok(())
}

fn neighbors_of(
    ctx: &ExecutionContext,
    vid: u64,
    label: &str,
    direction: Direction,
) -> StorageBackendResult<Vec<u64>> {
    let Some(graph) = ctx.graph.as_ref() else {
        return Err(runtime_model_error(
            "graph-neighbor lookup requires an execution graph",
        ));
    };
    graph.neighbors(vid, label, direction)
}

fn apply_propagate(
    edge_label: &str,
    aggregation: AggregationKind,
    direction: Direction,
    ctx: &ExecutionContext,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    if ctx.graph.is_none() {
        return Err(runtime_model_error(
            "graph propagation requires an execution graph",
        ));
    }
    // Convert channel 0 to a probability map.
    let mut prob_map: BTreeMap<u64, f64> = BTreeMap::new();
    for (did, vec) in &state.channel_map {
        prob_map.insert(*did, sigmoid(vec[0]));
    }
    // Discover neighbors of every existing doc to expand the working set.
    let mut all_vertices: std::collections::BTreeSet<u64> =
        state.channel_map.keys().copied().collect();
    for vid in state.channel_map.keys().copied().collect::<Vec<_>>() {
        for nb in neighbors_of(ctx, vid, edge_label, direction)? {
            all_vertices.insert(nb);
        }
    }
    let mut new_map: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    for vid in &all_vertices {
        let mut neighbor_probs: Vec<f64> = Vec::new();
        for nb in neighbors_of(ctx, *vid, edge_label, direction)? {
            if let Some(p) = prob_map.get(&nb) {
                neighbor_probs.push(*p);
            }
        }
        if neighbor_probs.is_empty() {
            if let Some(existing) = state.channel_map.get(vid) {
                let existing = crate::backend::try_clone_slice(
                    existing,
                    "graph-propagation unchanged channels",
                )
                .map_err(|error| runtime_model_error(error.to_string()))?;
                new_map.insert(*vid, existing);
            }
            continue;
        }
        let agg = match aggregation {
            AggregationKind::Mean => {
                neighbor_probs.iter().sum::<f64>()
                    / usize_to_f64_exact(neighbor_probs.len(), "graph-propagation neighbor count")?
            }
            AggregationKind::Sum => neighbor_probs.iter().sum::<f64>().min(1.0 - PROB_EPSILON),
            AggregationKind::Max => neighbor_probs
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max),
        };
        let propagated_logit = apply_gating(safe_logit(agg), gating);
        let mut new_vec = match state.channel_map.get(vid) {
            Some(existing) => {
                crate::backend::try_clone_slice(existing, "graph-propagation output channels")
                    .map_err(|error| runtime_model_error(error.to_string()))?
            }
            None => {
                runtime_filled_vec(state.num_channels, 0.0, "graph-propagation output channels")?
            }
        };
        if new_vec.is_empty() {
            new_vec =
                runtime_filled_vec(state.num_channels, 0.0, "graph-propagation output channels")?;
        }
        new_vec[0] += propagated_logit;
        new_map.insert(*vid, new_vec);
    }
    state.channel_map = new_map;
    Ok(())
}

fn apply_conv(
    edge_label: &str,
    hop_weights: &[f64],
    direction: Direction,
    ctx: &ExecutionContext,
    gating: Gating,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    if ctx.graph.is_none() {
        return Err(runtime_model_error(
            "graph convolution requires an execution graph",
        ));
    }
    if hop_weights.is_empty() {
        return Err(runtime_model_error(
            "graph convolution requires at least one hop weight",
        ));
    }
    let total_w: f64 = hop_weights.iter().sum();
    if !total_w.is_finite() || total_w <= 0.0 {
        return Err(runtime_model_error(
            "graph convolution hop weights must have a finite positive sum",
        ));
    }
    let mut norm = runtime_vec_with_capacity(hop_weights.len(), "graph-convolution weights")?;
    norm.extend(hop_weights.iter().map(|w| w / total_w));
    let mut val_map: BTreeMap<u64, f64> = BTreeMap::new();
    for (did, vec) in &state.channel_map {
        val_map.insert(*did, sigmoid(vec[0]));
    }
    let kernel_hops = hop_weights.len() - 1;
    let mut new_map: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    for vid in state.channel_map.keys().copied().collect::<Vec<_>>() {
        let mut weighted = 0.0f64;
        if let Some(p) = val_map.get(&vid) {
            weighted += norm[0] * p;
        }
        let mut current_frontier: std::collections::BTreeSet<u64> =
            std::collections::BTreeSet::from([vid]);
        let mut visited: std::collections::BTreeSet<u64> = current_frontier.clone();
        for hop_weight in norm.iter().copied().skip(1).take(kernel_hops) {
            let mut next_frontier: std::collections::BTreeSet<u64> =
                std::collections::BTreeSet::new();
            for fv in &current_frontier {
                for nb in neighbors_of(ctx, *fv, edge_label, direction)? {
                    if visited.insert(nb) {
                        next_frontier.insert(nb);
                    }
                }
            }
            if !next_frontier.is_empty() {
                let hop_vals: Vec<f64> = next_frontier
                    .iter()
                    .filter_map(|nb| val_map.get(nb).copied())
                    .collect();
                if !hop_vals.is_empty() {
                    let mean = hop_vals.iter().sum::<f64>()
                        / usize_to_f64_exact(hop_vals.len(), "graph-convolution hop count")?;
                    weighted += hop_weight * mean;
                }
            }
            current_frontier = next_frontier;
        }
        let conv_logit = apply_gating(
            safe_logit(weighted.clamp(PROB_EPSILON, 1.0 - PROB_EPSILON)),
            gating,
        );
        let mut new_vec = crate::backend::try_clone_slice(
            &state.channel_map[&vid],
            "graph-convolution output channels",
        )
        .map_err(|error| runtime_model_error(error.to_string()))?;
        new_vec[0] += conv_logit;
        new_map.insert(vid, new_vec);
    }
    state.channel_map = new_map;
    Ok(())
}

fn apply_pool(
    edge_label: &str,
    pool_size: usize,
    method: PoolMethod,
    direction: Direction,
    ctx: &ExecutionContext,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
    if ctx.graph.is_none() {
        return Err(runtime_model_error(
            "graph pooling requires an execution graph",
        ));
    }
    if pool_size == 1 {
        return Ok(());
    }
    if pool_size == 0 {
        return Err(runtime_model_error(
            "graph pooling size must be greater than zero",
        ));
    }
    let mut remaining: std::collections::BTreeSet<u64> =
        state.channel_map.keys().copied().collect();
    let mut pooled: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    while let Some(seed) = remaining.iter().copied().next() {
        remaining.remove(&seed);
        let mut group: Vec<u64> = vec![seed];
        let mut frontier: std::collections::BTreeSet<u64> =
            std::collections::BTreeSet::from([seed]);
        let mut visited: std::collections::BTreeSet<u64> = frontier.clone();
        while group.len() < pool_size && !frontier.is_empty() {
            let mut next: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
            for fv in &frontier {
                for nb in neighbors_of(ctx, *fv, edge_label, direction)? {
                    if visited.insert(nb) {
                        next.insert(nb);
                        if remaining.remove(&nb) {
                            group.push(nb);
                            if group.len() >= pool_size {
                                break;
                            }
                        }
                    }
                }
                if group.len() >= pool_size {
                    break;
                }
            }
            frontier = next;
        }
        let dim = state.channel_map.get(&seed).map_or(0, Vec::len);
        let mut agg = match method {
            PoolMethod::Avg => runtime_filled_vec(dim, 0.0f64, "graph-pool average")?,
            PoolMethod::Max => runtime_filled_vec(dim, f64::NEG_INFINITY, "graph-pool maximum")?,
        };
        for g in &group {
            let v = &state.channel_map[g];
            if v.len() != dim {
                return Err(runtime_model_error(format!(
                    "graph-pool input for doc {g} has {} channels, expected {dim}",
                    v.len()
                )));
            }
            for (i, slot) in agg.iter_mut().enumerate() {
                let x = v[i];
                match method {
                    PoolMethod::Avg => *slot += x,
                    PoolMethod::Max => {
                        if x > *slot {
                            *slot = x;
                        }
                    }
                }
            }
        }
        if matches!(method, PoolMethod::Avg) {
            let n = usize_to_f64_exact(group.len(), "graph-pool group size")?;
            for x in &mut agg {
                *x /= n;
            }
        }
        pooled.insert(seed, agg);
    }
    state.channel_map = pooled;
    Ok(())
}

fn apply_attention(state: &mut ForwardState) -> StorageBackendResult<()> {
    let ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if ids.len() < 2 {
        return Ok(());
    }
    let dim = state.channel_map[&ids[0]].len();
    if dim == 0 {
        return Ok(());
    }
    let scale = usize_to_f64_exact(dim, "attention channel count")?.sqrt();
    let mut xs = runtime_vec_with_capacity(ids.len(), "attention input rows")?;
    for id in &ids {
        xs.push(
            crate::backend::try_clone_slice(&state.channel_map[id], "attention input channels")
                .map_err(|error| runtime_model_error(error.to_string()))?,
        );
    }
    if let Some((row, values)) = xs
        .iter()
        .enumerate()
        .find(|(_, values)| values.len() != dim)
    {
        return Err(runtime_model_error(format!(
            "attention row {row} has {} channels, expected {dim}",
            values.len()
        )));
    }
    // Scaled dot-product attention with Q=K=V=X.
    let mut out_rows = runtime_vec_with_capacity(ids.len(), "attention output rows")?;
    for q in &xs {
        // Compute attention logits over every key.
        let mut logits = runtime_vec_with_capacity(xs.len(), "attention logits")?;
        for k in &xs {
            let dot: f64 = q.iter().zip(k.iter()).map(|(a, b)| a * b).sum();
            logits.push(dot / scale);
        }
        let max = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut exps = runtime_vec_with_capacity(logits.len(), "attention exponentials")?;
        exps.extend(logits.iter().map(|x| (x - max).exp()));
        let sum: f64 = exps.iter().sum();
        let weights: Vec<f64> = if sum > 0.0 {
            let mut weights = runtime_vec_with_capacity(exps.len(), "attention weights")?;
            weights.extend(exps.iter().map(|x| x / sum));
            weights
        } else {
            runtime_filled_vec(
                xs.len(),
                1.0 / usize_to_f64_exact(xs.len(), "attention row count")?,
                "attention fallback weights",
            )?
        };
        let mut combined = runtime_filled_vec(dim, 0.0f64, "attention combined channels")?;
        for (w, v) in weights.iter().zip(xs.iter()) {
            for i in 0..dim {
                combined[i] += w * v[i];
            }
        }
        out_rows.push(combined);
    }
    for (did, row) in ids.into_iter().zip(out_rows) {
        state.channel_map.insert(did, row);
    }
    Ok(())
}

fn build_result(
    channel_map: &BTreeMap<u64, Vec<f64>>,
    num_channels: usize,
    softmax_applied: bool,
) -> OperatorResult {
    if channel_map.is_empty() {
        return Ok(PostingList::new());
    }
    let mut entries = runtime_vec_with_capacity(channel_map.len(), "deep-fusion result entries")?;
    for (doc_id, vec) in channel_map {
        if vec.len() != num_channels || vec.is_empty() {
            return Err(runtime_model_error(format!(
                "deep-fusion result for doc {doc_id} has {} channels, expected {num_channels}",
                vec.len()
            )));
        }
        if softmax_applied {
            let score = vec.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mut payload = Payload::with_score(score);
            let mut class_probs = runtime_vec_with_capacity(vec.len(), "class probabilities")?;
            class_probs.extend(vec.iter().map(|value| Value::Float(*value)));
            payload
                .fields
                .insert("class_probs".into(), Value::List(class_probs));
            entries.push(PostingEntry::new(*doc_id, payload));
        } else if num_channels == 1 {
            let score = sigmoid(vec[0]);
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(score)));
        } else {
            let max_sigmoid = vec
                .iter()
                .map(|x| sigmoid(*x))
                .fold(f64::NEG_INFINITY, f64::max);
            entries.push(PostingEntry::new(*doc_id, Payload::with_score(max_sigmoid)));
        }
    }
    entries.sort_by_key(|e| e.doc_id);
    Ok(PostingList::from_sorted_unchecked(entries))
}

fn runtime_model_error(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

fn runtime_vec_with_capacity<T>(capacity: usize, context: &str) -> StorageBackendResult<Vec<T>> {
    try_vec_with_capacity(capacity, context).map_err(|error| runtime_model_error(error.to_string()))
}

fn runtime_filled_vec<T: Clone>(
    length: usize,
    value: T,
    context: &str,
) -> StorageBackendResult<Vec<T>> {
    try_filled_vec(length, value, context).map_err(|error| runtime_model_error(error.to_string()))
}

fn usize_to_f64_exact(value: usize, context: &str) -> StorageBackendResult<f64> {
    const MAX_EXACT_INTEGER: u64 = 9_007_199_254_740_992;
    let value = u64::try_from(value)
        .map_err(|_| runtime_model_error(format!("{context} exceeds the u64 bridge")))?;
    if value > MAX_EXACT_INTEGER {
        return Err(runtime_model_error(format!(
            "{context} exceeds f64's exact integer range"
        )));
    }
    Ok(value as f64)
}

fn safe_logit(p: f64) -> f64 {
    let clamped = p.clamp(PROB_EPSILON, 1.0 - PROB_EPSILON);
    logit(clamped)
}

fn apply_gating(x: f64, gating: Gating) -> f64 {
    match gating {
        Gating::None => x,
        Gating::Softplus => {
            if x > 20.0 {
                x
            } else if x < -20.0 {
                x.exp()
            } else {
                x.exp().ln_1p()
            }
        }
        Gating::Sigmoid => sigmoid(x),
        Gating::ReLU => x.max(0.0),
        Gating::Swish => x * sigmoid(x),
        Gating::Gelu => {
            let scaled = (2.0 / std::f64::consts::PI).sqrt() * (x + 0.044_715 * x * x * x);
            0.5 * x * (1.0 + scaled.tanh())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::Payload;
    use uqa_operators::GraphNeighborLookup;

    struct ConstOperator(Vec<PostingEntry>);

    impl Operator for ConstOperator {
        fn execute(&self, _ctx: &ExecutionContext) -> OperatorResult {
            Ok(PostingList::from_sorted_unchecked(self.0.clone()))
        }
    }

    fn entry(id: u64, score: f64) -> PostingEntry {
        PostingEntry::new(id, Payload::with_score(score))
    }

    /// Tiny adjacency-list neighbor lookup for the graph layer tests.
    struct StaticGraph {
        out_edges: BTreeMap<u64, Vec<(String, u64)>>,
    }

    impl StaticGraph {
        fn new() -> Self {
            Self {
                out_edges: BTreeMap::new(),
            }
        }

        fn add(&mut self, src: u64, label: &str, dst: u64) {
            self.out_edges
                .entry(src)
                .or_default()
                .push((label.to_string(), dst));
        }
    }

    impl GraphNeighborLookup for StaticGraph {
        fn neighbors(
            &self,
            vertex: u64,
            label: &str,
            direction: Direction,
        ) -> StorageBackendResult<Vec<u64>> {
            let mut out = Vec::new();
            if matches!(direction, Direction::Out | Direction::Both) {
                if let Some(es) = self.out_edges.get(&vertex) {
                    for (l, d) in es {
                        if label.is_empty() || l == label {
                            out.push(*d);
                        }
                    }
                }
            }
            if matches!(direction, Direction::In | Direction::Both) {
                for (src, es) in &self.out_edges {
                    for (l, d) in es {
                        if (label.is_empty() || l == label) && *d == vertex {
                            out.push(*src);
                        }
                    }
                }
            }
            out.sort_unstable();
            out.dedup();
            Ok(out)
        }
    }

    #[test]
    fn signal_only_pipeline_returns_sigmoid_of_logit() {
        let signal =
            Arc::new(ConstOperator(vec![entry(1, 0.8), entry(2, 0.2)])) as Arc<dyn Operator>;
        let op = DeepFusionOperator::new(vec![Layer::Signal(vec![signal])], 0.0, Gating::None)
            .expect("valid signal model");
        let result = op
            .execute(&ExecutionContext::new())
            .expect("signal-only deep fusion should execute");
        let scores: BTreeMap<u64, f64> = result
            .entries()
            .iter()
            .map(|e| (e.doc_id, e.payload.score))
            .collect();
        // Logit of 0.8 -> sigmoid back to ~0.8.
        assert!((scores[&1] - 0.8).abs() < 1e-6);
        assert!((scores[&2] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn embed_then_dense_then_softmax_classifies() {
        // Two-class linear classifier: x = [3, 1], W picks class 0.
        let layers = vec![
            Layer::Embed(vec![3.0, 1.0]),
            Layer::Flatten,
            Layer::Dense {
                weights: vec![1.0, 0.0, 0.0, 1.0],
                bias: vec![0.0, 0.0],
                output_channels: 2,
                input_channels: 2,
            },
            Layer::Softmax,
        ];
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid dense model");
        let result = op
            .execute(&ExecutionContext::new())
            .expect("dense deep fusion should execute");
        assert_eq!(result.entries().len(), 1);
        let payload = &result.entries()[0].payload;
        let probs = match payload.fields.get("class_probs") {
            Some(Value::List(items)) => items
                .iter()
                .filter_map(|v| match v {
                    Value::Float(f) => Some(*f),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => panic!("missing class_probs"),
        };
        assert_eq!(probs.len(), 2);
        assert!(probs[0] > probs[1], "expected class 0 to win: {probs:?}");
        assert!((probs.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn dropout_scales_inference_values() {
        let layers = vec![
            Layer::Embed(vec![2.0, 4.0]),
            Layer::Dropout { p: 0.5 },
            Layer::Flatten,
        ];
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid dropout model");
        let result = op
            .execute(&ExecutionContext::new())
            .expect("dropout deep fusion should execute");
        let entry = &result.entries()[0];
        // Final layer is Flatten so num_channels=2 and result reports
        // max-sigmoid on the post-dropout values [1.0, 2.0].
        let expected = [1.0_f64, 2.0_f64]
            .into_iter()
            .map(sigmoid)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!((entry.payload.score - expected).abs() < 1e-9);
    }

    #[test]
    fn cnn_1d_detects_adjacent_pattern() {
        let layers = vec![
            Layer::Embed(vec![1.0, 2.0, 3.0]),
            Layer::CNN1D {
                weights: vec![1.0, 1.0],
                bias: vec![0.0],
                output_channels: 1,
                input_channels: 1,
                kernel_size: 2,
                stride: 1,
                padding: 0,
            },
        ];
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid CNN1D model");
        let result = op
            .execute(&ExecutionContext::new())
            .expect("1-D convolution should execute");
        assert_eq!(result.entries().len(), 2);
        let scores: BTreeMap<u64, f64> = result
            .entries()
            .iter()
            .map(|e| (e.doc_id, e.payload.score))
            .collect();
        assert!((scores[&1] - sigmoid(3.0)).abs() < 1e-9);
        assert!((scores[&2] - sigmoid(5.0)).abs() < 1e-9);
    }

    #[test]
    fn cnn_2d_filters_flattened_image() {
        let layers = vec![
            Layer::Embed(vec![1.0, 2.0, 3.0, 4.0]),
            Layer::Flatten,
            Layer::CNN2D {
                weights: vec![1.0, 1.0, 1.0, 1.0],
                bias: vec![0.0],
                output_channels: 1,
                input_channels: 1,
                input_height: 2,
                input_width: 2,
                kernel_height: 2,
                kernel_width: 2,
                stride_height: 1,
                stride_width: 1,
                padding_height: 0,
                padding_width: 0,
            },
        ];
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid CNN2D model");
        let result = op
            .execute(&ExecutionContext::new())
            .expect("2-D convolution should execute");
        assert_eq!(result.entries().len(), 1);
        assert!((result.entries()[0].payload.score - sigmoid(10.0)).abs() < 1e-9);
    }

    #[test]
    fn cnn_2d_uses_channel_vectors_per_spatial_position() {
        let layers = vec![
            Layer::Embed(vec![1.0, 2.0]),
            Layer::Dense {
                weights: vec![1.0, 0.5],
                bias: vec![0.0, 0.0],
                output_channels: 2,
                input_channels: 1,
            },
            Layer::CNN2D {
                weights: vec![0.01, 0.02, 0.03, 0.04],
                bias: vec![0.0],
                output_channels: 1,
                input_channels: 2,
                input_height: 1,
                input_width: 2,
                kernel_height: 1,
                kernel_width: 2,
                stride_height: 1,
                stride_width: 1,
                padding_height: 0,
                padding_width: 0,
            },
        ];
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None)
            .expect("valid multi-channel CNN2D model");
        let result = op
            .execute(&ExecutionContext::new())
            .expect("multi-channel 2-D convolution should execute");
        assert_eq!(result.entries().len(), 1);
        assert!((result.entries()[0].payload.score - sigmoid(0.12)).abs() < 1e-9);
    }

    #[test]
    fn rnn_returns_sequence_hidden_states() {
        let layers = vec![
            Layer::Embed(vec![1.0, 1.0]),
            Layer::RNN {
                weights_input: vec![1.0],
                weights_hidden: vec![1.0],
                bias: vec![0.0],
                hidden_channels: 1,
                input_channels: 1,
                return_sequences: true,
            },
        ];
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid RNN model");
        let result = op
            .execute(&ExecutionContext::new())
            .expect("RNN deep fusion should execute");
        let first_hidden = 1.0_f64.tanh();
        let second_hidden = (1.0 + first_hidden).tanh();
        let scores: BTreeMap<u64, f64> = result
            .entries()
            .iter()
            .map(|e| (e.doc_id, e.payload.score))
            .collect();
        assert!((scores[&1] - sigmoid(first_hidden)).abs() < 1e-9);
        assert!((scores[&2] - sigmoid(second_hidden)).abs() < 1e-9);
    }

    #[test]
    fn lstm_accumulates_cell_state() {
        let layers = vec![
            Layer::Embed(vec![1.0, 1.0]),
            Layer::LSTM {
                weights_input: vec![10.0, 10.0, 1.0, 10.0],
                weights_hidden: vec![0.0; 4],
                bias: vec![0.0; 4],
                hidden_channels: 1,
                input_channels: 1,
                return_sequences: true,
            },
        ];
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid LSTM model");
        let result = op
            .execute(&ExecutionContext::new())
            .expect("LSTM deep fusion should execute");
        let scores: Vec<f64> = result.entries().iter().map(|e| e.payload.score).collect();
        assert_eq!(scores.len(), 2);
        assert!(scores[1] > scores[0], "{scores:?}");
    }

    #[test]
    fn propagate_lifts_neighbor_scores_into_residual() {
        // Build: signal entry on doc 1 (score 0.9). Doc 2 has no
        // signal. Add an edge 1->2 in the graph. After Propagate, doc
        // 2 should pick up a positive logit residual.
        let signal = Arc::new(ConstOperator(vec![entry(1, 0.9)])) as Arc<dyn Operator>;
        let mut graph = StaticGraph::new();
        graph.add(1, "knows", 2);
        let ctx = ExecutionContext::new().with_graph(Arc::new(graph));
        let layers = vec![
            Layer::Signal(vec![signal]),
            Layer::Propagate {
                edge_label: "knows".into(),
                aggregation: AggregationKind::Mean,
                direction: Direction::Both,
            },
        ];
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None)
            .expect("valid graph-propagation model");
        let result = op.execute(&ctx).expect("graph propagation should execute");
        let scores: BTreeMap<u64, f64> = result
            .entries()
            .iter()
            .map(|e| (e.doc_id, e.payload.score))
            .collect();
        assert!(scores.contains_key(&2));
        // Doc 2 had no signal, so its prior was 0. After propagation
        // from doc 1's neighbor probability (~0.9), the sigmoid score
        // on channel 0 should be > 0.5.
        assert!(scores[&2] > 0.5, "{scores:?}");
    }

    #[test]
    fn empty_edge_label_propagates_across_every_label() {
        let signal = Arc::new(ConstOperator(vec![entry(1, 0.9)])) as Arc<dyn Operator>;
        let mut graph = StaticGraph::new();
        graph.add(1, "knows", 2);
        graph.add(1, "likes", 3);
        let context = ExecutionContext::new().with_graph(Arc::new(graph));
        let layers = vec![
            Layer::Signal(vec![signal]),
            Layer::Propagate {
                edge_label: String::new(),
                aggregation: AggregationKind::Mean,
                direction: Direction::Both,
            },
        ];
        let operator = DeepFusionOperator::new(layers, 0.0, Gating::None)
            .expect("the empty edge-label wildcard is a valid model");
        let result = operator
            .execute(&context)
            .expect("the wildcard must traverse every edge label");
        let ids: Vec<u64> = result.entries().iter().map(|entry| entry.doc_id).collect();
        assert!(ids.contains(&2), "{ids:?}");
        assert!(ids.contains(&3), "{ids:?}");
    }

    #[test]
    fn pool_groups_neighbors_into_representative() {
        // Doc 1 and 2 are connected; pool groups them together. Doc 3
        // is isolated and forms its own group.
        let signal = Arc::new(ConstOperator(vec![
            entry(1, 0.8),
            entry(2, 0.6),
            entry(3, 0.5),
        ])) as Arc<dyn Operator>;
        let mut graph = StaticGraph::new();
        graph.add(1, "k", 2);
        let ctx = ExecutionContext::new().with_graph(Arc::new(graph));
        let layers = vec![
            Layer::Signal(vec![signal]),
            Layer::Pool {
                edge_label: "k".into(),
                pool_size: 2,
                method: PoolMethod::Avg,
                direction: Direction::Both,
            },
        ];
        let op =
            DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid graph-pool model");
        let result = op.execute(&ctx).expect("graph pooling should execute");
        let ids: Vec<u64> = result.entries().iter().map(|e| e.doc_id).collect();
        // After pooling, the representatives are {1, 3}; doc 2 is
        // absorbed into doc 1's group.
        assert!(ids.contains(&1));
        assert!(ids.contains(&3));
        assert!(!ids.contains(&2));
    }

    #[test]
    fn attention_keeps_node_count_and_normalises_within_node() {
        let layers = vec![Layer::Embed(vec![1.0, 0.5, -0.5]), Layer::Attention];
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None).expect("valid attention model");
        let result = op
            .execute(&ExecutionContext::new())
            .expect("attention deep fusion should execute");
        let ids: Vec<u64> = result.entries().iter().map(|e| e.doc_id).collect();
        assert_eq!(ids.len(), 3);
        // Self-attention over single-channel inputs leaves a weighted
        // mix of the same three values; sigmoids of those mixes should
        // remain in [0, 1].
        for entry in result.entries() {
            assert!((0.0..=1.0).contains(&entry.payload.score));
        }
    }

    #[test]
    fn invalid_model_shapes_are_rejected_at_construction() {
        let error = DeepFusionOperator::new(
            vec![
                Layer::Embed(vec![1.0, 2.0]),
                Layer::Dense {
                    weights: vec![1.0],
                    bias: vec![0.0],
                    output_channels: 1,
                    input_channels: 2,
                },
            ],
            0.0,
            Gating::None,
        )
        .err()
        .expect("a truncated dense matrix must be rejected");
        assert!(error
            .to_string()
            .contains("weights has length 1, expected 2"));
    }

    #[test]
    fn feature_models_reject_wrong_width_and_non_finite_inputs() {
        let operator =
            DeepFusionOperator::new(vec![Layer::Input { dimensions: 2 }], 0.0, Gating::None)
                .expect("valid feature model");
        let width_error = operator
            .execute_features(1, vec![1.0], &ExecutionContext::new())
            .expect_err("wrong-width features must not be padded");
        assert!(width_error.to_string().contains("dimension 1, expected 2"));

        let finite_error = operator
            .execute_features(1, vec![1.0, f64::NAN], &ExecutionContext::new())
            .expect_err("non-finite features must not reach inference");
        assert!(finite_error.to_string().contains("must be finite"));
    }

    #[test]
    fn graph_layers_require_an_execution_graph() {
        let signal = Arc::new(ConstOperator(vec![entry(1, 0.8)])) as Arc<dyn Operator>;
        let operator = DeepFusionOperator::new(
            vec![
                Layer::Signal(vec![signal]),
                Layer::Propagate {
                    edge_label: "knows".into(),
                    aggregation: AggregationKind::Mean,
                    direction: Direction::Out,
                },
            ],
            0.0,
            Gating::None,
        )
        .expect("valid graph model");
        let error = operator
            .execute(&ExecutionContext::new())
            .expect_err("missing graph context must not become a no-op");
        assert!(error.to_string().contains("requires an execution graph"));
    }

    #[test]
    fn non_finite_signal_scores_are_execution_errors() {
        let signal = Arc::new(ConstOperator(vec![entry(1, f64::NAN)])) as Arc<dyn Operator>;
        let operator =
            DeepFusionOperator::new(vec![Layer::Signal(vec![signal])], 0.0, Gating::None)
                .expect("valid signal model");
        let error = operator
            .execute(&ExecutionContext::new())
            .expect_err("non-finite signal output must not become a result score");
        assert!(error.to_string().contains("finite probability"));

        let out_of_range = Arc::new(ConstOperator(vec![entry(1, 1.5)])) as Arc<dyn Operator>;
        let operator =
            DeepFusionOperator::new(vec![Layer::Signal(vec![out_of_range])], 0.0, Gating::None)
                .expect("valid signal model");
        assert!(operator.execute(&ExecutionContext::new()).is_err());
    }
}
