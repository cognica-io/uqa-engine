//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed validation inputs share shape and numeric rules without copying model weights.

use crate::{DeepLayerSpec, Layer};

pub(super) enum ValidationLayer<'a> {
    Signal(usize),
    Embed(&'a [f64]),
    Other,
    Input {
        dimensions: usize,
    },
    Dense {
        weights: &'a [f64],
        bias: &'a [f64],
        output_channels: usize,
        input_channels: usize,
    },
    BatchNorm {
        epsilon: f64,
    },
    Dropout {
        p: f64,
    },
    CNN1D {
        weights: &'a [f64],
        bias: &'a [f64],
        output_channels: usize,
        input_channels: usize,
        kernel_size: usize,
        stride: usize,
        padding: usize,
    },
    CNN2D {
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
    },
    Conv {
        hop_weights: &'a [f64],
    },
    Pool {
        pool_size: usize,
    },
    Recurrent {
        weights_input: &'a [f64],
        weights_hidden: &'a [f64],
        bias: &'a [f64],
        hidden_channels: usize,
        input_channels: usize,
    },
    LongShortTermMemory {
        weights_input: &'a [f64],
        weights_hidden: &'a [f64],
        bias: &'a [f64],
        hidden_channels: usize,
        input_channels: usize,
    },
}

impl<'a> From<&'a Layer> for ValidationLayer<'a> {
    fn from(layer: &'a Layer) -> Self {
        match layer {
            Layer::Signal(signals) => Self::Signal(signals.len()),
            Layer::Embed(values) => Self::Embed(values),
            Layer::Input { dimensions, .. } => Self::Input {
                dimensions: *dimensions,
            },
            Layer::Dense {
                weights,
                bias,
                output_channels,
                input_channels,
                ..
            } => Self::Dense {
                weights,
                bias,
                output_channels: *output_channels,
                input_channels: *input_channels,
            },
            Layer::BatchNorm { epsilon, .. } => Self::BatchNorm { epsilon: *epsilon },
            Layer::Dropout { p, .. } => Self::Dropout { p: *p },
            Layer::CNN1D { .. } | Layer::CNN2D { .. } => Self::runtime_convolution(layer),
            Layer::Conv { hop_weights, .. } => Self::Conv { hop_weights },
            Layer::Pool { pool_size, .. } => Self::Pool {
                pool_size: *pool_size,
            },
            Layer::RNN {
                weights_input,
                weights_hidden,
                bias,
                hidden_channels,
                input_channels,
                ..
            } => Self::Recurrent {
                weights_input,
                weights_hidden,
                bias,
                hidden_channels: *hidden_channels,
                input_channels: *input_channels,
            },
            Layer::LSTM {
                weights_input,
                weights_hidden,
                bias,
                hidden_channels,
                input_channels,
                ..
            } => Self::LongShortTermMemory {
                weights_input,
                weights_hidden,
                bias,
                hidden_channels: *hidden_channels,
                input_channels: *input_channels,
            },
            Layer::Flatten
            | Layer::Softmax
            | Layer::Attention
            | Layer::Propagate { .. }
            | Layer::GlobalPool(_) => Self::Other,
        }
    }
}

impl<'a> From<&'a DeepLayerSpec> for ValidationLayer<'a> {
    fn from(layer: &'a DeepLayerSpec) -> Self {
        match layer {
            DeepLayerSpec::Embed { embedding } => Self::Embed(embedding),
            DeepLayerSpec::Input { dimensions, .. } => Self::Input {
                dimensions: *dimensions,
            },
            DeepLayerSpec::Dense {
                weights,
                bias,
                output_channels,
                input_channels,
                ..
            } => Self::Dense {
                weights,
                bias,
                output_channels: *output_channels,
                input_channels: *input_channels,
            },
            DeepLayerSpec::BatchNorm { epsilon, .. } => Self::BatchNorm { epsilon: *epsilon },
            DeepLayerSpec::Dropout { p, .. } => Self::Dropout { p: *p },
            DeepLayerSpec::CNN1D { .. } | DeepLayerSpec::CNN2D { .. } => {
                Self::model_convolution(layer)
            }
            DeepLayerSpec::Conv { hop_weights, .. } => Self::Conv { hop_weights },
            DeepLayerSpec::Pool { pool_size, .. } => Self::Pool {
                pool_size: *pool_size,
            },
            DeepLayerSpec::RNN {
                weights_input,
                weights_hidden,
                bias,
                hidden_channels,
                input_channels,
                ..
            } => Self::Recurrent {
                weights_input,
                weights_hidden,
                bias,
                hidden_channels: *hidden_channels,
                input_channels: *input_channels,
            },
            DeepLayerSpec::LSTM {
                weights_input,
                weights_hidden,
                bias,
                hidden_channels,
                input_channels,
                ..
            } => Self::LongShortTermMemory {
                weights_input,
                weights_hidden,
                bias,
                hidden_channels: *hidden_channels,
                input_channels: *input_channels,
            },
            DeepLayerSpec::Flatten
            | DeepLayerSpec::Softmax
            | DeepLayerSpec::Attention
            | DeepLayerSpec::Propagate { .. }
            | DeepLayerSpec::GlobalPool { .. } => Self::Other,
        }
    }
}

impl<'a> ValidationLayer<'a> {
    fn runtime_convolution(layer: &'a Layer) -> Self {
        match layer {
            Layer::CNN1D {
                weights,
                bias,
                output_channels,
                input_channels,
                kernel_size,
                stride,
                padding,
                ..
            } => Self::CNN1D {
                weights,
                bias,
                output_channels: *output_channels,
                input_channels: *input_channels,
                kernel_size: *kernel_size,
                stride: *stride,
                padding: *padding,
            },
            Layer::CNN2D {
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
                ..
            } => Self::CNN2D {
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
            _ => unreachable!("selected convolution layer"),
        }
    }
    fn model_convolution(layer: &'a DeepLayerSpec) -> Self {
        match layer {
            DeepLayerSpec::CNN1D {
                weights,
                bias,
                output_channels,
                input_channels,
                kernel_size,
                stride,
                padding,
                ..
            } => Self::CNN1D {
                weights,
                bias,
                output_channels: *output_channels,
                input_channels: *input_channels,
                kernel_size: *kernel_size,
                stride: *stride,
                padding: *padding,
            },
            DeepLayerSpec::CNN2D {
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
                ..
            } => Self::CNN2D {
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
            _ => unreachable!("selected convolution layer"),
        }
    }
}
