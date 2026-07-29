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

use uqa_operators::{base::Direction, ExecutionContext, Operator};

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
    pub layers: Vec<Layer>,
    pub alpha: f64,
    pub gating: Gating,
}

impl DeepFusionOperator {
    pub fn new(layers: Vec<Layer>, alpha: f64, gating: Gating) -> Self {
        assert!(
            !layers.is_empty(),
            "DeepFusionOperator requires at least one layer"
        );
        match &layers[0] {
            Layer::Signal(_) | Layer::Embed(_) | Layer::Input { .. } => {}
            _ => panic!("DeepFusionOperator: first layer must be Signal, Embed, or Input"),
        }
        Self {
            layers,
            alpha,
            gating,
        }
    }

    fn coverage_default(coverage: usize, total: usize) -> f64 {
        uqa_operators::hybrid::coverage_based_default(coverage, total, 0.01)
    }

    pub fn execute_features(
        &self,
        doc_id: u64,
        features: Vec<f64>,
        ctx: &ExecutionContext,
    ) -> PostingList {
        let mut state = ForwardState {
            num_channels: features.len(),
            channel_map: BTreeMap::from([(doc_id, features)]),
            softmax_applied: false,
        };
        self.apply_layers(ctx, &mut state);
        build_result(
            &state.channel_map,
            state.num_channels,
            state.softmax_applied,
        )
    }

    fn apply_layers(&self, ctx: &ExecutionContext, state: &mut ForwardState) {
        for layer in &self.layers {
            self.apply_layer(ctx, state, layer);
        }
    }

    fn apply_layer(&self, ctx: &ExecutionContext, state: &mut ForwardState, layer: &Layer) {
        match layer {
            Layer::Input { dimensions } => {
                state.num_channels = *dimensions;
            }
            Layer::Embed(embedding) => apply_embed(embedding, state),
            Layer::Signal(signals) => apply_signal(signals, ctx, self.alpha, self.gating, state),
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
            ),
            Layer::Flatten => apply_flatten(state),
            Layer::GlobalPool(method) => apply_global_pool(*method, state),
            Layer::Softmax => apply_softmax(state),
            Layer::BatchNorm { epsilon } => apply_batch_norm(*epsilon, state),
            Layer::Dropout { p } => apply_dropout(*p, state),
            Layer::CNN1D { .. } => {
                apply_cnn_1d(Convolution1D::from_layer(layer), self.gating, state);
            }
            Layer::CNN2D { .. } => {
                apply_cnn_2d(Convolution2D::from_layer(layer), self.gating, state);
            }
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
            ),
            Layer::Conv {
                edge_label,
                hop_weights,
                direction,
            } => apply_conv(edge_label, hop_weights, *direction, ctx, self.gating, state),
            Layer::Pool {
                edge_label,
                pool_size,
                method,
                direction,
            } => apply_pool(edge_label, *pool_size, *method, *direction, ctx, state),
            Layer::Attention => apply_attention(state),
            Layer::RNN { .. } => apply_rnn(Recurrent::from_layer(layer), state),
            Layer::LSTM { .. } => apply_lstm(LongShortTermMemory::from_layer(layer), state),
        }
    }
}

impl Operator for DeepFusionOperator {
    fn execute(&self, ctx: &ExecutionContext) -> PostingList {
        let mut state = ForwardState {
            channel_map: BTreeMap::new(),
            num_channels: 1,
            softmax_applied: false,
        };
        self.apply_layers(ctx, &mut state);
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
                } => total += (*output_channels * *input_channels) as f64,
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

impl<'a> Convolution1D<'a> {
    fn from_layer(layer: &'a Layer) -> Self {
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
            unreachable!("Convolution1D::from_layer requires Layer::CNN1D");
        };
        Self {
            weights,
            bias,
            output_channels: *output_channels,
            input_channels: *input_channels,
            kernel_size: *kernel_size,
            stride: *stride,
            padding: *padding,
        }
    }
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

impl<'a> Convolution2D<'a> {
    fn from_layer(layer: &'a Layer) -> Self {
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
            unreachable!("Convolution2D::from_layer requires Layer::CNN2D");
        };
        Self {
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
        }
    }
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

impl<'a> Recurrent<'a> {
    fn from_layer(layer: &'a Layer) -> Self {
        let Layer::RNN {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            return_sequences,
        } = layer
        else {
            unreachable!("Recurrent::from_layer requires Layer::RNN");
        };
        Self {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels: *hidden_channels,
            input_channels: *input_channels,
            return_sequences: *return_sequences,
        }
    }
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

impl<'a> LongShortTermMemory<'a> {
    fn from_layer(layer: &'a Layer) -> Self {
        let Layer::LSTM {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            return_sequences,
        } = layer
        else {
            unreachable!("LongShortTermMemory::from_layer requires Layer::LSTM");
        };
        Self {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels: *hidden_channels,
            input_channels: *input_channels,
            return_sequences: *return_sequences,
        }
    }
}

fn apply_embed(embedding: &[f64], state: &mut ForwardState) {
    for (i, val) in embedding.iter().enumerate() {
        state.channel_map.insert((i + 1) as u64, vec![*val]);
    }
}

fn apply_signal(
    signals: &[Arc<dyn Operator>],
    ctx: &ExecutionContext,
    alpha: f64,
    gating: Gating,
    state: &mut ForwardState,
) {
    let posting_lists: Vec<PostingList> = signals.iter().map(|s| s.execute(ctx)).collect();
    let mut score_maps: Vec<BTreeMap<u64, f64>> = Vec::new();
    let mut all_doc_ids: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    for pl in &posting_lists {
        let mut smap: BTreeMap<u64, f64> = BTreeMap::new();
        for entry in pl.entries() {
            smap.insert(entry.doc_id, entry.payload.score);
            all_doc_ids.insert(entry.doc_id);
        }
        score_maps.push(smap);
    }
    if all_doc_ids.is_empty() {
        return;
    }
    let total = all_doc_ids.len();
    let defaults: Vec<f64> = score_maps
        .iter()
        .map(|m| DeepFusionOperator::coverage_default(m.len(), total))
        .collect();
    for doc_id in &all_doc_ids {
        let probs: Vec<f64> = score_maps
            .iter()
            .enumerate()
            .map(|(i, m)| m.get(doc_id).copied().unwrap_or(defaults[i]))
            .collect();
        let fused = if probs.len() == 1 {
            probs[0]
        } else {
            let n = probs.len();
            let weights = vec![1.0 / n as f64; n];
            log_odds_conjunction_weighted(&probs, &weights, alpha).unwrap_or(0.5)
        };
        let layer_logit = apply_gating(safe_logit(fused), gating);
        let n = state.num_channels;
        let entry = state
            .channel_map
            .entry(*doc_id)
            .or_insert_with(|| vec![0.0; n]);
        entry[0] += layer_logit;
    }
}

fn apply_dense(
    weights: &[f64],
    bias: &[f64],
    output_channels: usize,
    input_channels: usize,
    gating: Gating,
    state: &mut ForwardState,
) {
    assert_eq!(weights.len(), output_channels * input_channels);
    assert_eq!(bias.len(), output_channels);
    let doc_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    for did in doc_ids {
        let input = state.channel_map.get(&did).cloned().unwrap_or_default();
        let mut out = vec![0.0f64; output_channels];
        for o in 0..output_channels {
            let mut acc = bias[o];
            for i in 0..input_channels {
                let inp = input.get(i).copied().unwrap_or(0.0);
                acc += weights[o * input_channels + i] * inp;
            }
            out[o] = apply_gating(acc, gating);
        }
        state.channel_map.insert(did, out);
    }
    state.num_channels = output_channels;
}

fn apply_flatten(state: &mut ForwardState) {
    let sorted_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if sorted_ids.is_empty() {
        return;
    }
    let mut flat: Vec<f64> = Vec::new();
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
}

fn apply_global_pool(method: GlobalPoolMethod, state: &mut ForwardState) {
    let sorted_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if sorted_ids.is_empty() {
        return;
    }
    let n_dims = state.channel_map[&sorted_ids[0]].len();
    let mut sums = vec![0.0f64; n_dims];
    let mut maxes = vec![f64::NEG_INFINITY; n_dims];
    for did in &sorted_ids {
        let v = &state.channel_map[did];
        for i in 0..n_dims {
            let x = v.get(i).copied().unwrap_or(0.0);
            sums[i] += x;
            if x > maxes[i] {
                maxes[i] = x;
            }
        }
    }
    let count = sorted_ids.len() as f64;
    let pooled: Vec<f64> = match method {
        GlobalPoolMethod::Avg => sums.iter().map(|s| s / count).collect(),
        GlobalPoolMethod::Max => maxes.clone(),
        GlobalPoolMethod::AvgMax => {
            let mut combined = Vec::with_capacity(2 * n_dims);
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
}

fn apply_softmax(state: &mut ForwardState) {
    for vec_ref in state.channel_map.values_mut() {
        let max = vec_ref.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut exps: Vec<f64> = vec_ref.iter().map(|x| (x - max).exp()).collect();
        let sum: f64 = exps.iter().sum();
        if sum > 0.0 {
            for x in &mut exps {
                *x /= sum;
            }
        } else {
            let n = exps.len() as f64;
            for x in &mut exps {
                *x = 1.0 / n;
            }
        }
        *vec_ref = exps;
    }
    state.softmax_applied = true;
}

fn apply_batch_norm(epsilon: f64, state: &mut ForwardState) {
    if state.channel_map.len() < 2 {
        return;
    }
    let dim = state.channel_map.values().next().map_or(0, Vec::len);
    if dim == 0 {
        return;
    }
    let mut means = vec![0.0f64; dim];
    for v in state.channel_map.values() {
        for i in 0..dim {
            means[i] += v[i];
        }
    }
    let n = state.channel_map.len() as f64;
    for m in &mut means {
        *m /= n;
    }
    let mut vars = vec![0.0f64; dim];
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
}

fn apply_dropout(p: f64, state: &mut ForwardState) {
    let scale = 1.0 - p;
    for v in state.channel_map.values_mut() {
        for x in v.iter_mut() {
            *x *= scale;
        }
    }
}

fn apply_cnn_1d(params: Convolution1D<'_>, gating: Gating, state: &mut ForwardState) {
    assert!(
        params.kernel_size > 0,
        "CNN1D kernel_size must be greater than zero"
    );
    assert!(params.stride > 0, "CNN1D stride must be greater than zero");
    assert_eq!(
        params.weights.len(),
        params.output_channels * params.kernel_size * params.input_channels
    );
    assert_eq!(params.bias.len(), params.output_channels);
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return;
    }
    let padded_len = input_ids.len() + 2 * params.padding;
    if padded_len < params.kernel_size {
        state.channel_map.clear();
        state.num_channels = params.output_channels;
        return;
    }
    let output_len = (padded_len - params.kernel_size) / params.stride + 1;
    let base_doc_id = input_ids[0];
    let mut output = BTreeMap::new();
    for out_pos in 0..output_len {
        let mut row = vec![0.0f64; params.output_channels];
        for (out_ch, slot) in row.iter_mut().enumerate() {
            let mut acc = params.bias[out_ch];
            for kernel_pos in 0..params.kernel_size {
                let raw_pos = out_pos * params.stride + kernel_pos;
                if raw_pos < params.padding {
                    continue;
                }
                let input_pos = raw_pos - params.padding;
                if input_pos >= input_ids.len() {
                    continue;
                }
                let input_vec = &state.channel_map[&input_ids[input_pos]];
                for in_ch in 0..params.input_channels {
                    let weight_index =
                        (out_ch * params.kernel_size + kernel_pos) * params.input_channels + in_ch;
                    acc +=
                        params.weights[weight_index] * input_vec.get(in_ch).copied().unwrap_or(0.0);
                }
            }
            *slot = apply_gating(acc, gating);
        }
        let doc_id = input_ids
            .get(out_pos)
            .copied()
            .unwrap_or(base_doc_id + out_pos as u64);
        output.insert(doc_id, row);
    }
    state.channel_map = output;
    state.num_channels = params.output_channels;
    state.softmax_applied = false;
}

fn apply_cnn_2d(params: Convolution2D<'_>, gating: Gating, state: &mut ForwardState) {
    assert!(
        params.input_height > 0,
        "CNN2D input_height must be greater than zero"
    );
    assert!(
        params.input_width > 0,
        "CNN2D input_width must be greater than zero"
    );
    assert!(
        params.kernel_height > 0,
        "CNN2D kernel_height must be greater than zero"
    );
    assert!(
        params.kernel_width > 0,
        "CNN2D kernel_width must be greater than zero"
    );
    assert!(
        params.stride_height > 0,
        "CNN2D stride_height must be greater than zero"
    );
    assert!(
        params.stride_width > 0,
        "CNN2D stride_width must be greater than zero"
    );
    assert_eq!(
        params.weights.len(),
        params.output_channels * params.kernel_height * params.kernel_width * params.input_channels
    );
    assert_eq!(params.bias.len(), params.output_channels);
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return;
    }
    let mut flat_input = Vec::new();
    if input_ids.len() == 1 {
        flat_input.extend_from_slice(&state.channel_map[&input_ids[0]]);
    } else {
        for doc_id in &input_ids {
            flat_input.extend_from_slice(&state.channel_map[doc_id]);
        }
    }

    let padded_height = params.input_height + 2 * params.padding_height;
    let padded_width = params.input_width + 2 * params.padding_width;
    if padded_height < params.kernel_height || padded_width < params.kernel_width {
        state.channel_map.clear();
        state.num_channels = params.output_channels;
        return;
    }
    let output_height = (padded_height - params.kernel_height) / params.stride_height + 1;
    let output_width = (padded_width - params.kernel_width) / params.stride_width + 1;
    let base_doc_id = input_ids[0];
    let mut output = BTreeMap::new();

    for out_row in 0..output_height {
        for out_col in 0..output_width {
            let mut row = vec![0.0f64; params.output_channels];
            for (out_ch, slot) in row.iter_mut().enumerate() {
                *slot = apply_gating(
                    cnn_2d_cell(&params, &flat_input, out_row, out_col, out_ch),
                    gating,
                );
            }
            let output_index = out_row * output_width + out_col;
            let doc_id = input_ids
                .get(output_index)
                .copied()
                .unwrap_or(base_doc_id + output_index as u64);
            output.insert(doc_id, row);
        }
    }
    state.channel_map = output;
    state.num_channels = params.output_channels;
    state.softmax_applied = false;
}

fn cnn_2d_cell(
    params: &Convolution2D<'_>,
    flat_input: &[f64],
    out_row: usize,
    out_col: usize,
    out_ch: usize,
) -> f64 {
    let mut acc = params.bias[out_ch];
    for kernel_row in 0..params.kernel_height {
        let raw_row = out_row * params.stride_height + kernel_row;
        if raw_row < params.padding_height {
            continue;
        }
        let input_row = raw_row - params.padding_height;
        if input_row >= params.input_height {
            continue;
        }
        for kernel_col in 0..params.kernel_width {
            let raw_col = out_col * params.stride_width + kernel_col;
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
                acc += params.weights[weight_index]
                    * flat_input.get(input_index).copied().unwrap_or(0.0);
            }
        }
    }
    acc
}

fn apply_rnn(params: Recurrent<'_>, state: &mut ForwardState) {
    assert!(
        params.hidden_channels > 0,
        "RNN hidden_channels must be greater than zero"
    );
    assert!(
        params.input_channels > 0,
        "RNN input_channels must be greater than zero"
    );
    assert_eq!(
        params.weights_input.len(),
        params.hidden_channels * params.input_channels
    );
    assert_eq!(
        params.weights_hidden.len(),
        params.hidden_channels * params.hidden_channels
    );
    assert_eq!(params.bias.len(), params.hidden_channels);
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return;
    }
    let mut hidden = vec![0.0f64; params.hidden_channels];
    let mut output = BTreeMap::new();
    for doc_id in &input_ids {
        let input = &state.channel_map[doc_id];
        let mut next_hidden = vec![0.0f64; params.hidden_channels];
        for (out_ch, slot) in next_hidden.iter_mut().enumerate() {
            let mut acc = params.bias[out_ch];
            for in_ch in 0..params.input_channels {
                acc += params.weights_input[out_ch * params.input_channels + in_ch]
                    * input.get(in_ch).copied().unwrap_or(0.0);
            }
            for (hidden_ch, hidden_value) in hidden.iter().enumerate() {
                acc += params.weights_hidden[out_ch * params.hidden_channels + hidden_ch]
                    * hidden_value;
            }
            *slot = acc.tanh();
        }
        hidden = next_hidden;
        if params.return_sequences {
            output.insert(*doc_id, hidden.clone());
        }
    }
    if !params.return_sequences {
        output.insert(*input_ids.last().expect("non-empty input"), hidden);
    }
    state.channel_map = output;
    state.num_channels = params.hidden_channels;
    state.softmax_applied = false;
}

fn apply_lstm(params: LongShortTermMemory<'_>, state: &mut ForwardState) {
    assert!(
        params.hidden_channels > 0,
        "LSTM hidden_channels must be greater than zero"
    );
    assert!(
        params.input_channels > 0,
        "LSTM input_channels must be greater than zero"
    );
    let gate_channels = 4 * params.hidden_channels;
    assert_eq!(
        params.weights_input.len(),
        gate_channels * params.input_channels
    );
    assert_eq!(
        params.weights_hidden.len(),
        gate_channels * params.hidden_channels
    );
    assert_eq!(params.bias.len(), gate_channels);
    let input_ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if input_ids.is_empty() {
        return;
    }
    let mut hidden = vec![0.0f64; params.hidden_channels];
    let mut cell = vec![0.0f64; params.hidden_channels];
    let mut output = BTreeMap::new();
    for doc_id in &input_ids {
        let input = &state.channel_map[doc_id];
        let mut gates = vec![0.0f64; gate_channels];
        for (gate_ch, gate_slot) in gates.iter_mut().enumerate() {
            let mut acc = params.bias[gate_ch];
            for in_ch in 0..params.input_channels {
                acc += params.weights_input[gate_ch * params.input_channels + in_ch]
                    * input.get(in_ch).copied().unwrap_or(0.0);
            }
            for (hidden_ch, hidden_value) in hidden.iter().enumerate() {
                acc += params.weights_hidden[gate_ch * params.hidden_channels + hidden_ch]
                    * hidden_value;
            }
            *gate_slot = acc;
        }

        let mut next_hidden = vec![0.0f64; params.hidden_channels];
        let mut next_cell = vec![0.0f64; params.hidden_channels];
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
            output.insert(*doc_id, hidden.clone());
        }
    }
    if !params.return_sequences {
        output.insert(*input_ids.last().expect("non-empty input"), hidden);
    }
    state.channel_map = output;
    state.num_channels = params.hidden_channels;
    state.softmax_applied = false;
}

fn neighbors_of(ctx: &ExecutionContext, vid: u64, label: &str, direction: Direction) -> Vec<u64> {
    let Some(graph) = ctx.graph.as_ref() else {
        return Vec::new();
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
) {
    if ctx.graph.is_none() {
        return;
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
        for nb in neighbors_of(ctx, vid, edge_label, direction) {
            all_vertices.insert(nb);
        }
    }
    let mut new_map: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    for vid in &all_vertices {
        let mut neighbor_probs: Vec<f64> = Vec::new();
        for nb in neighbors_of(ctx, *vid, edge_label, direction) {
            if let Some(p) = prob_map.get(&nb) {
                neighbor_probs.push(*p);
            }
        }
        if neighbor_probs.is_empty() {
            if let Some(existing) = state.channel_map.get(vid) {
                new_map.insert(*vid, existing.clone());
            }
            continue;
        }
        let agg = match aggregation {
            AggregationKind::Mean => {
                neighbor_probs.iter().sum::<f64>() / neighbor_probs.len() as f64
            }
            AggregationKind::Sum => neighbor_probs.iter().sum::<f64>().min(1.0 - PROB_EPSILON),
            AggregationKind::Max => neighbor_probs
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max),
        };
        let propagated_logit = apply_gating(safe_logit(agg), gating);
        let mut new_vec = state
            .channel_map
            .get(vid)
            .cloned()
            .unwrap_or_else(|| vec![0.0; state.num_channels]);
        if new_vec.is_empty() {
            new_vec = vec![0.0; state.num_channels];
        }
        new_vec[0] += propagated_logit;
        new_map.insert(*vid, new_vec);
    }
    state.channel_map = new_map;
}

fn apply_conv(
    edge_label: &str,
    hop_weights: &[f64],
    direction: Direction,
    ctx: &ExecutionContext,
    gating: Gating,
    state: &mut ForwardState,
) {
    if ctx.graph.is_none() || hop_weights.is_empty() {
        return;
    }
    let total_w: f64 = hop_weights.iter().sum();
    if total_w <= 0.0 {
        return;
    }
    let norm: Vec<f64> = hop_weights.iter().map(|w| w / total_w).collect();
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
                for nb in neighbors_of(ctx, *fv, edge_label, direction) {
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
                    let mean = hop_vals.iter().sum::<f64>() / hop_vals.len() as f64;
                    weighted += hop_weight * mean;
                }
            }
            current_frontier = next_frontier;
        }
        let conv_logit = apply_gating(
            safe_logit(weighted.clamp(PROB_EPSILON, 1.0 - PROB_EPSILON)),
            gating,
        );
        let mut new_vec = state.channel_map[&vid].clone();
        new_vec[0] += conv_logit;
        new_map.insert(vid, new_vec);
    }
    state.channel_map = new_map;
}

fn apply_pool(
    edge_label: &str,
    pool_size: usize,
    method: PoolMethod,
    direction: Direction,
    ctx: &ExecutionContext,
    state: &mut ForwardState,
) {
    if ctx.graph.is_none() || pool_size < 2 {
        return;
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
                for nb in neighbors_of(ctx, *fv, edge_label, direction) {
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
            PoolMethod::Avg => vec![0.0f64; dim],
            PoolMethod::Max => vec![f64::NEG_INFINITY; dim],
        };
        for g in &group {
            let v = &state.channel_map[g];
            for (i, slot) in agg.iter_mut().enumerate() {
                let x = v.get(i).copied().unwrap_or(0.0);
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
            let n = group.len() as f64;
            for x in &mut agg {
                *x /= n;
            }
        }
        let rep = *group.iter().min().unwrap_or(&seed);
        pooled.insert(rep, agg);
    }
    state.channel_map = pooled;
}

fn apply_attention(state: &mut ForwardState) {
    let ids: Vec<u64> = state.channel_map.keys().copied().collect();
    if ids.len() < 2 {
        return;
    }
    let dim = state.channel_map[&ids[0]].len();
    if dim == 0 {
        return;
    }
    let scale = (dim as f64).sqrt();
    let xs: Vec<Vec<f64>> = ids.iter().map(|id| state.channel_map[id].clone()).collect();
    // Scaled dot-product attention with Q=K=V=X.
    let mut out_rows: Vec<Vec<f64>> = Vec::with_capacity(ids.len());
    for q in &xs {
        // Compute attention logits over every key.
        let mut logits: Vec<f64> = Vec::with_capacity(xs.len());
        for k in &xs {
            let dot: f64 = q.iter().zip(k.iter()).map(|(a, b)| a * b).sum();
            logits.push(dot / scale);
        }
        let max = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let exps: Vec<f64> = logits.iter().map(|x| (x - max).exp()).collect();
        let sum: f64 = exps.iter().sum();
        let weights: Vec<f64> = if sum > 0.0 {
            exps.iter().map(|x| x / sum).collect()
        } else {
            vec![1.0 / xs.len() as f64; xs.len()]
        };
        let mut combined = vec![0.0f64; dim];
        for (w, v) in weights.iter().zip(xs.iter()) {
            for i in 0..dim {
                combined[i] += w * v[i];
            }
        }
        out_rows.push(combined);
    }
    for (i, did) in ids.iter().enumerate() {
        state.channel_map.insert(*did, out_rows[i].clone());
    }
}

fn build_result(
    channel_map: &BTreeMap<u64, Vec<f64>>,
    num_channels: usize,
    softmax_applied: bool,
) -> PostingList {
    if channel_map.is_empty() {
        return PostingList::new();
    }
    let mut entries: Vec<PostingEntry> = Vec::with_capacity(channel_map.len());
    for (doc_id, vec) in channel_map {
        if softmax_applied {
            let score = vec.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mut payload = Payload::with_score(score);
            payload.fields.insert(
                "class_probs".into(),
                Value::List(vec.iter().map(|v| Value::Float(*v)).collect()),
            );
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
    PostingList::from_sorted_unchecked(entries)
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
        fn execute(&self, _ctx: &ExecutionContext) -> PostingList {
            PostingList::from_sorted_unchecked(self.0.clone())
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
        fn neighbors(&self, vertex: u64, label: &str, direction: Direction) -> Vec<u64> {
            let mut out = Vec::new();
            if matches!(direction, Direction::Out | Direction::Both) {
                if let Some(es) = self.out_edges.get(&vertex) {
                    for (l, d) in es {
                        if l == label {
                            out.push(*d);
                        }
                    }
                }
            }
            if matches!(direction, Direction::In | Direction::Both) {
                for (src, es) in &self.out_edges {
                    for (l, d) in es {
                        if l == label && *d == vertex {
                            out.push(*src);
                        }
                    }
                }
            }
            out.sort_unstable();
            out.dedup();
            out
        }
    }

    #[test]
    fn signal_only_pipeline_returns_sigmoid_of_logit() {
        let signal =
            Arc::new(ConstOperator(vec![entry(1, 0.8), entry(2, 0.2)])) as Arc<dyn Operator>;
        let op = DeepFusionOperator::new(vec![Layer::Signal(vec![signal])], 0.0, Gating::None);
        let result = op.execute(&ExecutionContext::new());
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
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None);
        let result = op.execute(&ExecutionContext::new());
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
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None);
        let result = op.execute(&ExecutionContext::new());
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
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None);
        let result = op.execute(&ExecutionContext::new());
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
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None);
        let result = op.execute(&ExecutionContext::new());
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
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None);
        let result = op.execute(&ExecutionContext::new());
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
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None);
        let result = op.execute(&ExecutionContext::new());
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
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None);
        let result = op.execute(&ExecutionContext::new());
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
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None);
        let result = op.execute(&ctx);
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
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None);
        let result = op.execute(&ctx);
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
        let op = DeepFusionOperator::new(layers, 0.0, Gating::None);
        let result = op.execute(&ExecutionContext::new());
        let ids: Vec<u64> = result.entries().iter().map(|e| e.doc_id).collect();
        assert_eq!(ids.len(), 3);
        // Self-attention over single-channel inputs leaves a weighted
        // mix of the same three values; sigmoids of those mixes should
        // remain in [0, 1].
        for entry in result.entries() {
            assert!((0.0..=1.0).contains(&entry.payload.score));
        }
    }
}
