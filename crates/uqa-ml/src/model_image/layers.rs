//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{floats, invalid, ImageOutput, ModelInput, StorageBackendResult};
use crate::{AggregationSpec, DeepLayerSpec, DirectionSpec, PoolKindSpec, PoolMethodSpec};

pub(super) fn write(
    layer: &DeepLayerSpec,
    output: &mut ImageOutput<'_>,
) -> StorageBackendResult<()> {
    match layer {
        DeepLayerSpec::CNN1D { .. } | DeepLayerSpec::CNN2D { .. } => {
            write_convolution(layer, output)
        }
        DeepLayerSpec::Propagate { .. }
        | DeepLayerSpec::Conv { .. }
        | DeepLayerSpec::Pool { .. } => write_graph(layer, output),
        DeepLayerSpec::RNN { .. } | DeepLayerSpec::LSTM { .. } => write_recurrent(layer, output),
        DeepLayerSpec::Input { .. }
        | DeepLayerSpec::Embed { .. }
        | DeepLayerSpec::Dense { .. }
        | DeepLayerSpec::Flatten
        | DeepLayerSpec::GlobalPool { .. }
        | DeepLayerSpec::Softmax
        | DeepLayerSpec::BatchNorm { .. }
        | DeepLayerSpec::Dropout { .. }
        | DeepLayerSpec::Attention => write_linear(layer, output),
    }
}

fn write_linear(layer: &DeepLayerSpec, output: &mut ImageOutput<'_>) -> StorageBackendResult<()> {
    match layer {
        DeepLayerSpec::Input { dimensions } => {
            output.bytes(&[0])?;
            output.count(*dimensions)?;
        }
        DeepLayerSpec::Embed { embedding } => {
            output.bytes(&[1])?;
            floats(output, embedding)?;
        }
        DeepLayerSpec::Dense {
            weights,
            bias,
            output_channels,
            input_channels,
        } => {
            output.bytes(&[2])?;
            floats(output, weights)?;
            floats(output, bias)?;
            output.count(*output_channels)?;
            output.count(*input_channels)?;
        }
        DeepLayerSpec::Flatten => {
            output.bytes(&[3])?;
        }
        DeepLayerSpec::GlobalPool { method } => {
            output.bytes(&[4])?;
            output.bytes(&[match method {
                PoolMethodSpec::Avg => 0,
                PoolMethodSpec::Max => 1,
                PoolMethodSpec::AvgMax => 2,
            }])?;
        }
        DeepLayerSpec::Softmax => {
            output.bytes(&[5])?;
        }
        DeepLayerSpec::BatchNorm { epsilon } => {
            output.bytes(&[6])?;
            output.bytes(&epsilon.to_bits().to_le_bytes())?;
        }
        DeepLayerSpec::Dropout { p } => {
            output.bytes(&[7])?;
            output.bytes(&p.to_bits().to_le_bytes())?;
        }
        DeepLayerSpec::Attention => {
            output.bytes(&[13])?;
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

fn write_convolution(
    layer: &DeepLayerSpec,
    output: &mut ImageOutput<'_>,
) -> StorageBackendResult<()> {
    match layer {
        DeepLayerSpec::CNN1D {
            weights,
            bias,
            output_channels,
            input_channels,
            kernel_size,
            stride,
            padding,
        } => {
            output.bytes(&[8])?;
            floats(output, weights)?;
            floats(output, bias)?;
            output.count(*output_channels)?;
            output.count(*input_channels)?;
            output.count(*kernel_size)?;
            output.count(*stride)?;
            output.count(*padding)?;
        }
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
        } => {
            output.bytes(&[9])?;
            floats(output, weights)?;
            floats(output, bias)?;
            output.count(*output_channels)?;
            output.count(*input_channels)?;
            output.count(*input_height)?;
            output.count(*input_width)?;
            output.count(*kernel_height)?;
            output.count(*kernel_width)?;
            output.count(*stride_height)?;
            output.count(*stride_width)?;
            output.count(*padding_height)?;
            output.count(*padding_width)?;
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

fn write_graph(layer: &DeepLayerSpec, output: &mut ImageOutput<'_>) -> StorageBackendResult<()> {
    match layer {
        DeepLayerSpec::Propagate {
            edge_label,
            aggregation,
            direction,
        } => {
            output.bytes(&[10])?;
            output.sized(edge_label.as_bytes())?;
            output.bytes(&[match aggregation {
                AggregationSpec::Mean => 0,
                AggregationSpec::Sum => 1,
                AggregationSpec::Max => 2,
            }])?;
            output.bytes(&[match direction {
                DirectionSpec::Out => 0,
                DirectionSpec::In => 1,
                DirectionSpec::Both => 2,
            }])?;
        }
        DeepLayerSpec::Conv {
            edge_label,
            hop_weights,
            direction,
        } => {
            output.bytes(&[11])?;
            output.sized(edge_label.as_bytes())?;
            floats(output, hop_weights)?;
            output.bytes(&[match direction {
                DirectionSpec::Out => 0,
                DirectionSpec::In => 1,
                DirectionSpec::Both => 2,
            }])?;
        }
        DeepLayerSpec::Pool {
            edge_label,
            pool_size,
            method,
            direction,
        } => {
            output.bytes(&[12])?;
            output.sized(edge_label.as_bytes())?;
            output.count(*pool_size)?;
            output.bytes(&[match method {
                PoolKindSpec::Avg => 0,
                PoolKindSpec::Max => 1,
            }])?;
            output.bytes(&[match direction {
                DirectionSpec::Out => 0,
                DirectionSpec::In => 1,
                DirectionSpec::Both => 2,
            }])?;
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

fn write_recurrent(
    layer: &DeepLayerSpec,
    output: &mut ImageOutput<'_>,
) -> StorageBackendResult<()> {
    match layer {
        DeepLayerSpec::RNN {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            return_sequences,
        } => {
            output.bytes(&[14])?;
            floats(output, weights_input)?;
            floats(output, weights_hidden)?;
            floats(output, bias)?;
            output.count(*hidden_channels)?;
            output.count(*input_channels)?;
            output.bytes(&[u8::from(*return_sequences)])?;
        }
        DeepLayerSpec::LSTM {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            return_sequences,
        } => {
            output.bytes(&[15])?;
            floats(output, weights_input)?;
            floats(output, weights_hidden)?;
            floats(output, bias)?;
            output.count(*hidden_channels)?;
            output.count(*input_channels)?;
            output.bytes(&[u8::from(*return_sequences)])?;
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

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
