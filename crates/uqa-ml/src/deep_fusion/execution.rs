//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Feature and posting execution plus the physical operator contract.

use super::{
    apply_attention, apply_batch_norm, apply_cnn_1d_layer, apply_cnn_2d_layer, apply_conv,
    apply_dense, apply_dropout, apply_embed, apply_flatten, apply_global_pool, apply_lstm_layer,
    apply_pool, apply_propagate, apply_rnn_layer, apply_signal, apply_softmax, build_result,
    runtime_model_error, validate_state, BTreeMap, DeepFusionOperator, ExecutionContext,
    ForwardState, IndexStats, Layer, Operator, OperatorResult, StorageBackendError,
    StorageBackendResult,
};

impl DeepFusionOperator {
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

    pub(super) fn coverage_default(coverage: usize, total: usize) -> f64 {
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
