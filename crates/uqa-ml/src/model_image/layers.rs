//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, ModelInput, StorageBackendResult};
use crate::{AggregationSpec, DeepLayerSpec, DirectionSpec, PoolKindSpec, PoolMethodSpec};

mod fields;
pub(super) use fields::{fields, Field};

pub(super) fn read(input: &mut ModelInput<'_>) -> StorageBackendResult<DeepLayerSpec> {
    let tag = input.io.array::<1>()?[0];
    Ok(match tag {
        0 => DeepLayerSpec::Input {
            dimensions: input.io.count()?,
        },
        1 => DeepLayerSpec::Embed {
            embedding: input.floats()?,
        },
        2 => DeepLayerSpec::Dense {
            weights: input.floats()?,
            bias: input.floats()?,
            output_channels: input.io.count()?,
            input_channels: input.io.count()?,
        },
        3 => DeepLayerSpec::Flatten,
        4 => DeepLayerSpec::GlobalPool {
            method: match input.io.array::<1>()?[0] {
                0 => PoolMethodSpec::Avg,
                1 => PoolMethodSpec::Max,
                2 => PoolMethodSpec::AvgMax,
                _ => return Err(invalid()),
            },
        },
        5 => DeepLayerSpec::Softmax,
        6 => DeepLayerSpec::BatchNorm {
            epsilon: input.float()?,
        },
        7 => DeepLayerSpec::Dropout { p: input.float()? },
        8 | 9 => read_convolution(tag, input)?,
        10 => DeepLayerSpec::Propagate {
            edge_label: input.text()?,
            aggregation: match input.io.array::<1>()?[0] {
                0 => AggregationSpec::Mean,
                1 => AggregationSpec::Sum,
                2 => AggregationSpec::Max,
                _ => return Err(invalid()),
            },
            direction: match input.io.array::<1>()?[0] {
                0 => DirectionSpec::Out,
                1 => DirectionSpec::In,
                2 => DirectionSpec::Both,
                _ => return Err(invalid()),
            },
        },
        11 => DeepLayerSpec::Conv {
            edge_label: input.text()?,
            hop_weights: input.floats()?,
            direction: match input.io.array::<1>()?[0] {
                0 => DirectionSpec::Out,
                1 => DirectionSpec::In,
                2 => DirectionSpec::Both,
                _ => return Err(invalid()),
            },
        },
        12 => DeepLayerSpec::Pool {
            edge_label: input.text()?,
            pool_size: input.io.count()?,
            method: match input.io.array::<1>()?[0] {
                0 => PoolKindSpec::Avg,
                1 => PoolKindSpec::Max,
                _ => return Err(invalid()),
            },
            direction: match input.io.array::<1>()?[0] {
                0 => DirectionSpec::Out,
                1 => DirectionSpec::In,
                2 => DirectionSpec::Both,
                _ => return Err(invalid()),
            },
        },
        13 => DeepLayerSpec::Attention,
        14 => DeepLayerSpec::RNN {
            weights_input: input.floats()?,
            weights_hidden: input.floats()?,
            bias: input.floats()?,
            hidden_channels: input.io.count()?,
            input_channels: input.io.count()?,
            return_sequences: input.boolean()?,
        },
        15 => DeepLayerSpec::LSTM {
            weights_input: input.floats()?,
            weights_hidden: input.floats()?,
            bias: input.floats()?,
            hidden_channels: input.io.count()?,
            input_channels: input.io.count()?,
            return_sequences: input.boolean()?,
        },
        _ => return Err(invalid()),
    })
}

fn read_convolution(tag: u8, input: &mut ModelInput<'_>) -> StorageBackendResult<DeepLayerSpec> {
    Ok(match tag {
        8 => DeepLayerSpec::CNN1D {
            weights: input.floats()?,
            bias: input.floats()?,
            output_channels: input.io.count()?,
            input_channels: input.io.count()?,
            kernel_size: input.io.count()?,
            stride: input.io.count()?,
            padding: input.io.count()?,
        },
        9 => DeepLayerSpec::CNN2D {
            weights: input.floats()?,
            bias: input.floats()?,
            output_channels: input.io.count()?,
            input_channels: input.io.count()?,
            input_height: input.io.count()?,
            input_width: input.io.count()?,
            kernel_height: input.io.count()?,
            kernel_width: input.io.count()?,
            stride_height: input.io.count()?,
            stride_width: input.io.count()?,
            padding_height: input.io.count()?,
            padding_width: input.io.count()?,
        },
        _ => return Err(invalid()),
    })
}
