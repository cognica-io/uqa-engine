//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each stored layer projects a fixed number of exact wire fields without copying payloads.

use crate::{AggregationSpec, DeepLayerSpec, DirectionSpec, PoolKindSpec, PoolMethodSpec};

#[derive(Clone, Copy)]
pub(crate) enum Field<'a> {
    End,
    Byte(u8),
    Count(usize),
    Float(f64),
    Text(&'a str),
    Floats(&'a [f64]),
}

use Field::{Byte, Count, Float, Floats, Text};

fn record<const N: usize>(values: [Field<'_>; N]) -> [Field<'_>; 14] {
    let mut fields = [Field::End; 14];
    fields[..N].copy_from_slice(&values);
    fields
}

pub(crate) fn fields(layer: &DeepLayerSpec) -> [Field<'_>; 14] {
    match layer {
        DeepLayerSpec::Input { dimensions } => record([Byte(0), Count(*dimensions)]),
        DeepLayerSpec::Embed { embedding } => record([Byte(1), Floats(embedding)]),
        DeepLayerSpec::Dense {
            weights,
            bias,
            output_channels,
            input_channels,
        } => record([
            Byte(2),
            Floats(weights),
            Floats(bias),
            Count(*output_channels),
            Count(*input_channels),
        ]),
        DeepLayerSpec::Flatten => record([Byte(3)]),
        DeepLayerSpec::GlobalPool { method } => record([
            Byte(4),
            Byte(match method {
                PoolMethodSpec::Avg => 0,
                PoolMethodSpec::Max => 1,
                PoolMethodSpec::AvgMax => 2,
            }),
        ]),
        DeepLayerSpec::Softmax => record([Byte(5)]),
        DeepLayerSpec::BatchNorm { epsilon } => record([Byte(6), Float(*epsilon)]),
        DeepLayerSpec::Dropout { p } => record([Byte(7), Float(*p)]),
        DeepLayerSpec::CNN1D { .. } | DeepLayerSpec::CNN2D { .. } => convolution_fields(layer),
        DeepLayerSpec::Propagate {
            edge_label,
            aggregation,
            direction,
        } => record([
            Byte(10),
            Text(edge_label),
            Byte(match aggregation {
                AggregationSpec::Mean => 0,
                AggregationSpec::Sum => 1,
                AggregationSpec::Max => 2,
            }),
            Byte(direction_tag(*direction)),
        ]),
        DeepLayerSpec::Conv {
            edge_label,
            hop_weights,
            direction,
        } => record([
            Byte(11),
            Text(edge_label),
            Floats(hop_weights),
            Byte(direction_tag(*direction)),
        ]),
        DeepLayerSpec::Pool {
            edge_label,
            pool_size,
            method,
            direction,
        } => record([
            Byte(12),
            Text(edge_label),
            Count(*pool_size),
            Byte(match method {
                PoolKindSpec::Avg => 0,
                PoolKindSpec::Max => 1,
            }),
            Byte(direction_tag(*direction)),
        ]),
        DeepLayerSpec::Attention => record([Byte(13)]),
        DeepLayerSpec::RNN { .. } | DeepLayerSpec::LSTM { .. } => recurrent_fields(layer),
    }
}

fn convolution_fields(layer: &DeepLayerSpec) -> [Field<'_>; 14] {
    match layer {
        DeepLayerSpec::CNN1D {
            weights,
            bias,
            output_channels,
            input_channels,
            kernel_size,
            stride,
            padding,
        } => record([
            Byte(8),
            Floats(weights),
            Floats(bias),
            Count(*output_channels),
            Count(*input_channels),
            Count(*kernel_size),
            Count(*stride),
            Count(*padding),
        ]),
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
        } => record([
            Byte(9),
            Floats(weights),
            Floats(bias),
            Count(*output_channels),
            Count(*input_channels),
            Count(*input_height),
            Count(*input_width),
            Count(*kernel_height),
            Count(*kernel_width),
            Count(*stride_height),
            Count(*stride_width),
            Count(*padding_height),
            Count(*padding_width),
        ]),
        _ => unreachable!("convolution layer projection"),
    }
}

fn recurrent_fields(layer: &DeepLayerSpec) -> [Field<'_>; 14] {
    match layer {
        DeepLayerSpec::RNN {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            return_sequences,
        } => record([
            Byte(14),
            Floats(weights_input),
            Floats(weights_hidden),
            Floats(bias),
            Count(*hidden_channels),
            Count(*input_channels),
            Byte(u8::from(*return_sequences)),
        ]),
        DeepLayerSpec::LSTM {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            return_sequences,
        } => record([
            Byte(15),
            Floats(weights_input),
            Floats(weights_hidden),
            Floats(bias),
            Count(*hidden_channels),
            Count(*input_channels),
            Byte(u8::from(*return_sequences)),
        ]),
        _ => unreachable!("recurrent layer projection"),
    }
}

fn direction_tag(direction: DirectionSpec) -> u8 {
    match direction {
        DirectionSpec::Out => 0,
        DirectionSpec::In => 1,
        DirectionSpec::Both => 2,
    }
}
