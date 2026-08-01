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
}

mod attention;
mod cnn;
mod execution;
mod graph_layers;
mod layer_dispatch;
mod recurrent;
mod runtime;
mod state;
mod tensor_layers;
mod validation;

use attention::{apply_attention, build_result};
use cnn::{apply_cnn_1d, apply_cnn_2d};
use graph_layers::{apply_conv, apply_pool, apply_propagate};
use layer_dispatch::{apply_cnn_1d_layer, apply_cnn_2d_layer, apply_lstm_layer, apply_rnn_layer};
use recurrent::{apply_lstm, apply_rnn};
use runtime::{
    apply_gating, runtime_filled_vec, runtime_model_error, runtime_vec_with_capacity, safe_logit,
    usize_to_f64_exact,
};
use state::{Convolution1D, Convolution2D, ForwardState, LongShortTermMemory, Recurrent};
use tensor_layers::{
    apply_batch_norm, apply_dense, apply_dropout, apply_embed, apply_flatten, apply_global_pool,
    apply_signal, apply_softmax,
};
use validation::{validate_layers, validate_state};

#[cfg(test)]
mod tests;
