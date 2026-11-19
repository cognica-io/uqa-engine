//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{model, DeepLayerSpec, DeepModel};
use crate::{AggregationSpec, DirectionSpec, PoolKindSpec, PoolMethodSpec};

pub(super) fn models() -> Vec<DeepModel> {
    let mut layers = vec![
        DeepLayerSpec::Dense {
            weights: vec![-0.0, f64::from_bits(1)],
            bias: vec![f64::MIN_POSITIVE],
            output_channels: 1,
            input_channels: 2,
        },
        DeepLayerSpec::Flatten,
        DeepLayerSpec::Softmax,
        DeepLayerSpec::BatchNorm { epsilon: 0.001 },
        DeepLayerSpec::Dropout { p: 0.75 },
        DeepLayerSpec::Attention,
        DeepLayerSpec::CNN1D {
            weights: vec![0.5],
            bias: vec![-0.0],
            output_channels: 1,
            input_channels: 1,
            kernel_size: 1,
            stride: 2,
            padding: 1,
        },
        DeepLayerSpec::CNN2D {
            weights: vec![0.5],
            bias: vec![-0.0],
            output_channels: 1,
            input_channels: 1,
            input_height: 3,
            input_width: 5,
            kernel_height: 1,
            kernel_width: 1,
            stride_height: 2,
            stride_width: 3,
            padding_height: 1,
            padding_width: 2,
        },
    ];
    for method in [
        PoolMethodSpec::Avg,
        PoolMethodSpec::Max,
        PoolMethodSpec::AvgMax,
    ] {
        layers.push(DeepLayerSpec::GlobalPool { method });
    }
    for direction in [DirectionSpec::Out, DirectionSpec::In, DirectionSpec::Both] {
        for aggregation in [
            AggregationSpec::Mean,
            AggregationSpec::Sum,
            AggregationSpec::Max,
        ] {
            layers.push(DeepLayerSpec::Propagate {
                edge_label: "edge\0α".into(),
                aggregation,
                direction,
            });
        }
        layers.push(DeepLayerSpec::Conv {
            edge_label: "ordered hops".into(),
            hop_weights: vec![0.25, 0.75],
            direction,
        });
        for method in [PoolKindSpec::Avg, PoolKindSpec::Max] {
            layers.push(DeepLayerSpec::Pool {
                edge_label: "pool".into(),
                pool_size: 3,
                method,
                direction,
            });
        }
    }
    for return_sequences in [false, true] {
        layers.push(DeepLayerSpec::RNN {
            weights_input: vec![0.25],
            weights_hidden: vec![-0.5],
            bias: vec![0.0],
            hidden_channels: 1,
            input_channels: 1,
            return_sequences,
        });
        layers.push(DeepLayerSpec::LSTM {
            weights_input: vec![0.25; 4],
            weights_hidden: vec![-0.5; 4],
            bias: vec![0.0; 4],
            hidden_channels: 1,
            input_channels: 1,
            return_sequences,
        });
    }
    let mut models: Vec<_> = layers
        .into_iter()
        .map(|layer| model(vec![DeepLayerSpec::Input { dimensions: 2 }, layer]))
        .collect();
    models.push(model(vec![DeepLayerSpec::Input { dimensions: 7 }]));
    models.push(model(vec![DeepLayerSpec::Embed {
        embedding: vec![-0.0, f64::from_bits(1), f64::MAX],
    }]));
    models
}
