//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Both stored JSON inputs use the same layer fields and declaration order.

macro_rules! layer_fields {
    ($kind:expr, $field:ident, $unknown:expr) => {
        match $kind {
            "input" => crate::DeepLayerSpec::Input {
                dimensions: $field!("input", "dimensions", count)?,
            },
            "embed" => crate::DeepLayerSpec::Embed {
                embedding: $field!("embed", "embedding", floats)?,
            },
            "dense" => crate::DeepLayerSpec::Dense {
                weights: $field!("dense", "weights", floats)?,
                bias: $field!("dense", "bias", floats)?,
                output_channels: $field!("dense", "output_channels", count)?,
                input_channels: $field!("dense", "input_channels", count)?,
            },
            "flatten" => crate::DeepLayerSpec::Flatten,
            "global_pool" => crate::DeepLayerSpec::GlobalPool {
                method: $field!("global_pool", "method", global_pool)?,
            },
            "softmax" => crate::DeepLayerSpec::Softmax,
            "batch_norm" => crate::DeepLayerSpec::BatchNorm {
                epsilon: $field!("batch_norm", "epsilon", float)?,
            },
            "dropout" => crate::DeepLayerSpec::Dropout {
                p: $field!("dropout", "p", float)?,
            },
            "cnn_1d" => crate::DeepLayerSpec::CNN1D {
                weights: $field!("cnn_1d", "weights", floats)?,
                bias: $field!("cnn_1d", "bias", floats)?,
                output_channels: $field!("cnn_1d", "output_channels", count)?,
                input_channels: $field!("cnn_1d", "input_channels", count)?,
                kernel_size: $field!("cnn_1d", "kernel_size", count)?,
                stride: $field!("cnn_1d", "stride", count)?,
                padding: $field!("cnn_1d", "padding", count)?,
            },
            "cnn_2d" => crate::DeepLayerSpec::CNN2D {
                weights: $field!("cnn_2d", "weights", floats)?,
                bias: $field!("cnn_2d", "bias", floats)?,
                output_channels: $field!("cnn_2d", "output_channels", count)?,
                input_channels: $field!("cnn_2d", "input_channels", count)?,
                input_height: $field!("cnn_2d", "input_height", count)?,
                input_width: $field!("cnn_2d", "input_width", count)?,
                kernel_height: $field!("cnn_2d", "kernel_height", count)?,
                kernel_width: $field!("cnn_2d", "kernel_width", count)?,
                stride_height: $field!("cnn_2d", "stride_height", count)?,
                stride_width: $field!("cnn_2d", "stride_width", count)?,
                padding_height: $field!("cnn_2d", "padding_height", count)?,
                padding_width: $field!("cnn_2d", "padding_width", count)?,
            },
            "propagate" => crate::DeepLayerSpec::Propagate {
                edge_label: $field!("propagate", "edge_label", text)?,
                aggregation: $field!("propagate", "aggregation", aggregation)?,
                direction: $field!("propagate", "direction", direction)?,
            },
            "conv" => crate::DeepLayerSpec::Conv {
                edge_label: $field!("conv", "edge_label", text)?,
                hop_weights: $field!("conv", "hop_weights", floats)?,
                direction: $field!("conv", "direction", direction)?,
            },
            "pool" => crate::DeepLayerSpec::Pool {
                edge_label: $field!("pool", "edge_label", text)?,
                pool_size: $field!("pool", "pool_size", count)?,
                method: $field!("pool", "method", pool)?,
                direction: $field!("pool", "direction", direction)?,
            },
            "attention" => crate::DeepLayerSpec::Attention,
            "rnn" => crate::DeepLayerSpec::RNN {
                weights_input: $field!("rnn", "weights_input", floats)?,
                weights_hidden: $field!("rnn", "weights_hidden", floats)?,
                bias: $field!("rnn", "bias", floats)?,
                hidden_channels: $field!("rnn", "hidden_channels", count)?,
                input_channels: $field!("rnn", "input_channels", count)?,
                return_sequences: $field!("rnn", "return_sequences", boolean)?,
            },
            "lstm" => crate::DeepLayerSpec::LSTM {
                weights_input: $field!("lstm", "weights_input", floats)?,
                weights_hidden: $field!("lstm", "weights_hidden", floats)?,
                bias: $field!("lstm", "bias", floats)?,
                hidden_channels: $field!("lstm", "hidden_channels", count)?,
                input_channels: $field!("lstm", "input_channels", count)?,
                return_sequences: $field!("lstm", "return_sequences", boolean)?,
            },
            _ => $unknown,
        }
    };
}

pub(crate) use layer_fields;
