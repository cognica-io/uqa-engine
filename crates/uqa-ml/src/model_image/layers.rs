//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, ModelInput, StorageBackendResult};
use crate::{AggregationSpec, DeepLayerSpec, DirectionSpec, PoolKindSpec, PoolMethodSpec};

mod fields;
pub(super) use fields::{fields, Field};

pub(super) async fn read(input: &mut ModelInput<'_, '_>) -> StorageBackendResult<DeepLayerSpec> {
    let tag = input.io.array::<1>().await?[0];
    Ok(match tag {
        0 => DeepLayerSpec::Input {
            dimensions: input.io.count().await?,
        },
        1 => DeepLayerSpec::Embed {
            embedding: input.floats().await?,
        },
        2 => DeepLayerSpec::Dense {
            weights: input.floats().await?,
            bias: input.floats().await?,
            output_channels: input.io.count().await?,
            input_channels: input.io.count().await?,
        },
        3 => DeepLayerSpec::Flatten,
        4 => DeepLayerSpec::GlobalPool {
            method: match input.io.array::<1>().await?[0] {
                0 => PoolMethodSpec::Avg,
                1 => PoolMethodSpec::Max,
                2 => PoolMethodSpec::AvgMax,
                _ => return Err(invalid()),
            },
        },
        5 => DeepLayerSpec::Softmax,
        6 => DeepLayerSpec::BatchNorm {
            epsilon: input.float().await?,
        },
        7 => DeepLayerSpec::Dropout {
            p: input.float().await?,
        },
        8 | 9 => read_convolution(tag, input).await?,
        10 => DeepLayerSpec::Propagate {
            edge_label: input.text().await?,
            aggregation: match input.io.array::<1>().await?[0] {
                0 => AggregationSpec::Mean,
                1 => AggregationSpec::Sum,
                2 => AggregationSpec::Max,
                _ => return Err(invalid()),
            },
            direction: match input.io.array::<1>().await?[0] {
                0 => DirectionSpec::Out,
                1 => DirectionSpec::In,
                2 => DirectionSpec::Both,
                _ => return Err(invalid()),
            },
        },
        11 => DeepLayerSpec::Conv {
            edge_label: input.text().await?,
            hop_weights: input.floats().await?,
            direction: match input.io.array::<1>().await?[0] {
                0 => DirectionSpec::Out,
                1 => DirectionSpec::In,
                2 => DirectionSpec::Both,
                _ => return Err(invalid()),
            },
        },
        12 => DeepLayerSpec::Pool {
            edge_label: input.text().await?,
            pool_size: input.io.count().await?,
            method: match input.io.array::<1>().await?[0] {
                0 => PoolKindSpec::Avg,
                1 => PoolKindSpec::Max,
                _ => return Err(invalid()),
            },
            direction: match input.io.array::<1>().await?[0] {
                0 => DirectionSpec::Out,
                1 => DirectionSpec::In,
                2 => DirectionSpec::Both,
                _ => return Err(invalid()),
            },
        },
        13 => DeepLayerSpec::Attention,
        14 => DeepLayerSpec::RNN {
            weights_input: input.floats().await?,
            weights_hidden: input.floats().await?,
            bias: input.floats().await?,
            hidden_channels: input.io.count().await?,
            input_channels: input.io.count().await?,
            return_sequences: input.boolean().await?,
        },
        15 => DeepLayerSpec::LSTM {
            weights_input: input.floats().await?,
            weights_hidden: input.floats().await?,
            bias: input.floats().await?,
            hidden_channels: input.io.count().await?,
            input_channels: input.io.count().await?,
            return_sequences: input.boolean().await?,
        },
        _ => return Err(invalid()),
    })
}

async fn read_convolution(
    tag: u8,
    input: &mut ModelInput<'_, '_>,
) -> StorageBackendResult<DeepLayerSpec> {
    Ok(match tag {
        8 => DeepLayerSpec::CNN1D {
            weights: input.floats().await?,
            bias: input.floats().await?,
            output_channels: input.io.count().await?,
            input_channels: input.io.count().await?,
            kernel_size: input.io.count().await?,
            stride: input.io.count().await?,
            padding: input.io.count().await?,
        },
        9 => DeepLayerSpec::CNN2D {
            weights: input.floats().await?,
            bias: input.floats().await?,
            output_channels: input.io.count().await?,
            input_channels: input.io.count().await?,
            input_height: input.io.count().await?,
            input_width: input.io.count().await?,
            kernel_height: input.io.count().await?,
            kernel_width: input.io.count().await?,
            stride_height: input.io.count().await?,
            stride_width: input.io.count().await?,
            padding_height: input.io.count().await?,
            padding_width: input.io.count().await?,
        },
        _ => return Err(invalid()),
    })
}
