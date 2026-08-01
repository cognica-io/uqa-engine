//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Layer variant dispatch into typed runtime kernels.

use super::{
    apply_cnn_1d, apply_cnn_2d, apply_lstm, apply_rnn, runtime_model_error, Convolution1D,
    Convolution2D, ForwardState, Gating, Layer, LongShortTermMemory, Recurrent,
    StorageBackendResult,
};

pub(super) fn apply_cnn_1d_layer(
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

pub(super) fn apply_cnn_2d_layer(
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

pub(super) fn apply_rnn_layer(layer: &Layer, state: &mut ForwardState) -> StorageBackendResult<()> {
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

pub(super) fn apply_lstm_layer(
    layer: &Layer,
    state: &mut ForwardState,
) -> StorageBackendResult<()> {
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
