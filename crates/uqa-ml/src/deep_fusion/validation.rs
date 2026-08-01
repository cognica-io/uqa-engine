//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Model shape, weight, recurrent, and runtime-state validation.

use super::{ForwardState, Layer, MLError, MLResult, StorageBackendError, StorageBackendResult};

pub(super) fn validate_layers(layers: &[Layer], alpha: f64) -> MLResult<()> {
    let Some(first) = layers.first() else {
        return Err(MLError::InvalidModel(
            "DeepFusionOperator requires at least one layer".into(),
        ));
    };
    if !matches!(
        first,
        Layer::Signal(_) | Layer::Embed(_) | Layer::Input { .. }
    ) {
        return Err(MLError::InvalidModel(
            "the first deep-fusion layer must be Signal, Embed, or Input".into(),
        ));
    }
    if !alpha.is_finite() {
        return Err(MLError::InvalidModel(format!(
            "deep-fusion alpha must be finite, got {alpha}"
        )));
    }

    for (index, layer) in layers.iter().enumerate() {
        validate_layer(index, layer)?;
    }
    Ok(())
}

pub(super) fn validate_layer(index: usize, layer: &Layer) -> MLResult<()> {
    let context = format!("deep-fusion layer {index}");
    match layer {
        Layer::Input { dimensions } => validate_input(index, *dimensions, &context),
        Layer::Signal(signals) if signals.is_empty() => Err(MLError::InvalidModel(format!(
            "{context}: Signal requires at least one operator"
        ))),
        Layer::Embed(values) => validate_embedding(index, values, &context),
        Layer::Dense {
            weights,
            bias,
            output_channels,
            input_channels,
        } => validate_dense(weights, bias, *output_channels, *input_channels, &context),
        Layer::BatchNorm { epsilon } if !epsilon.is_finite() || *epsilon <= 0.0 => {
            Err(MLError::InvalidModel(format!(
                "{context}: epsilon must be finite and greater than zero"
            )))
        }
        Layer::Dropout { p } if !p.is_finite() || !(0.0..=1.0).contains(p) => {
            Err(MLError::InvalidModel(format!(
                "{context}: dropout probability must be finite and in [0, 1]"
            )))
        }
        Layer::CNN1D { .. } => validate_cnn_1d(layer, &context),
        Layer::CNN2D { .. } => validate_cnn_2d(layer, &context),
        Layer::Conv { hop_weights, .. } => validate_hop_weights(hop_weights, &context),
        Layer::Pool { pool_size, .. } => require_nonzero(*pool_size, &context, "pool_size"),
        Layer::RNN { .. } => validate_recurrent_spec(layer, 1, &context),
        Layer::LSTM { .. } => validate_recurrent_spec(layer, 4, &context),
        Layer::Signal(_)
        | Layer::BatchNorm { .. }
        | Layer::Dropout { .. }
        | Layer::Propagate { .. }
        | Layer::Flatten
        | Layer::GlobalPool(_)
        | Layer::Softmax
        | Layer::Attention => Ok(()),
    }
}

pub(super) fn validate_input(index: usize, dimensions: usize, context: &str) -> MLResult<()> {
    if index != 0 {
        return Err(MLError::InvalidModel(format!(
            "{context}: Input is only valid as the first layer"
        )));
    }
    require_nonzero(dimensions, context, "dimensions")
}

pub(super) fn validate_embedding(index: usize, values: &[f64], context: &str) -> MLResult<()> {
    if index != 0 {
        return Err(MLError::InvalidModel(format!(
            "{context}: Embed is only valid as the first layer"
        )));
    }
    if values.is_empty() {
        return Err(MLError::InvalidModel(format!(
            "{context}: embedding must not be empty"
        )));
    }
    require_finite(values, context, "embedding")
}

pub(super) fn validate_dense(
    weights: &[f64],
    bias: &[f64],
    output_channels: usize,
    input_channels: usize,
    context: &str,
) -> MLResult<()> {
    require_nonzero(output_channels, context, "output_channels")?;
    require_nonzero(input_channels, context, "input_channels")?;
    let expected = checked_product(
        &[output_channels, input_channels],
        context,
        "dense weight count",
    )?;
    require_len(weights.len(), expected, context, "weights")?;
    require_len(bias.len(), output_channels, context, "bias")?;
    require_finite(weights, context, "weights")?;
    require_finite(bias, context, "bias")
}

pub(super) fn validate_cnn_1d(layer: &Layer, context: &str) -> MLResult<()> {
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
        return Err(MLError::InvalidModel(format!(
            "{context}: internal CNN1D validation mismatch"
        )));
    };
    for (name, value) in [
        ("output_channels", *output_channels),
        ("input_channels", *input_channels),
        ("kernel_size", *kernel_size),
        ("stride", *stride),
    ] {
        require_nonzero(value, context, name)?;
    }
    padding
        .checked_mul(2)
        .ok_or_else(|| MLError::InvalidModel(format!("{context}: padding is too large")))?;
    let expected = checked_product(
        &[*output_channels, *kernel_size, *input_channels],
        context,
        "CNN1D weight count",
    )?;
    validate_weights_and_bias(weights, bias, expected, *output_channels, context)
}

pub(super) fn validate_cnn_2d(layer: &Layer, context: &str) -> MLResult<()> {
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
        return Err(MLError::InvalidModel(format!(
            "{context}: internal CNN2D validation mismatch"
        )));
    };
    for (name, value) in [
        ("output_channels", *output_channels),
        ("input_channels", *input_channels),
        ("input_height", *input_height),
        ("input_width", *input_width),
        ("kernel_height", *kernel_height),
        ("kernel_width", *kernel_width),
        ("stride_height", *stride_height),
        ("stride_width", *stride_width),
    ] {
        require_nonzero(value, context, name)?;
    }
    padding_height
        .checked_mul(2)
        .ok_or_else(|| MLError::InvalidModel(format!("{context}: padding_height is too large")))?;
    padding_width
        .checked_mul(2)
        .ok_or_else(|| MLError::InvalidModel(format!("{context}: padding_width is too large")))?;
    checked_product(
        &[*input_height, *input_width, *input_channels],
        context,
        "CNN2D input size",
    )?;
    let expected = checked_product(
        &[
            *output_channels,
            *kernel_height,
            *kernel_width,
            *input_channels,
        ],
        context,
        "CNN2D weight count",
    )?;
    validate_weights_and_bias(weights, bias, expected, *output_channels, context)
}

pub(super) fn validate_weights_and_bias(
    weights: &[f64],
    bias: &[f64],
    expected_weights: usize,
    output_channels: usize,
    context: &str,
) -> MLResult<()> {
    require_len(weights.len(), expected_weights, context, "weights")?;
    require_len(bias.len(), output_channels, context, "bias")?;
    require_finite(weights, context, "weights")?;
    require_finite(bias, context, "bias")
}

pub(super) fn validate_hop_weights(hop_weights: &[f64], context: &str) -> MLResult<()> {
    if hop_weights.is_empty() {
        return Err(MLError::InvalidModel(format!(
            "{context}: graph convolution requires at least one hop weight"
        )));
    }
    require_finite(hop_weights, context, "hop_weights")?;
    let total_weight: f64 = hop_weights.iter().sum();
    if hop_weights.iter().any(|weight| *weight < 0.0)
        || !total_weight.is_finite()
        || total_weight <= 0.0
    {
        return Err(MLError::InvalidModel(format!(
            "{context}: hop_weights must be non-negative with a finite positive sum"
        )));
    }
    Ok(())
}

pub(super) fn validate_recurrent_spec(layer: &Layer, gates: usize, context: &str) -> MLResult<()> {
    let (weights_input, weights_hidden, bias, hidden_channels, input_channels) = match layer {
        Layer::RNN {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            ..
        }
        | Layer::LSTM {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            ..
        } => (
            weights_input,
            weights_hidden,
            bias,
            *hidden_channels,
            *input_channels,
        ),
        _ => {
            return Err(MLError::InvalidModel(format!(
                "{context}: internal recurrent validation mismatch"
            )));
        }
    };
    validate_recurrent_layer(
        weights_input,
        weights_hidden,
        bias,
        hidden_channels,
        input_channels,
        gates,
        context,
    )
}

pub(super) fn validate_recurrent_layer(
    weights_input: &[f64],
    weights_hidden: &[f64],
    bias: &[f64],
    hidden_channels: usize,
    input_channels: usize,
    gates: usize,
    context: &str,
) -> MLResult<()> {
    require_nonzero(hidden_channels, context, "hidden_channels")?;
    require_nonzero(input_channels, context, "input_channels")?;
    let gate_channels = checked_product(&[gates, hidden_channels], context, "gate channels")?;
    let input_count = checked_product(
        &[gate_channels, input_channels],
        context,
        "input weight count",
    )?;
    let hidden_count = checked_product(
        &[gate_channels, hidden_channels],
        context,
        "hidden weight count",
    )?;
    require_len(weights_input.len(), input_count, context, "weights_input")?;
    require_len(
        weights_hidden.len(),
        hidden_count,
        context,
        "weights_hidden",
    )?;
    require_len(bias.len(), gate_channels, context, "bias")?;
    require_finite(weights_input, context, "weights_input")?;
    require_finite(weights_hidden, context, "weights_hidden")?;
    require_finite(bias, context, "bias")
}

pub(super) fn require_nonzero(value: usize, context: &str, field: &str) -> MLResult<()> {
    if value == 0 {
        Err(MLError::InvalidModel(format!(
            "{context}: {field} must be greater than zero"
        )))
    } else {
        Ok(())
    }
}

pub(super) fn require_len(
    actual: usize,
    expected: usize,
    context: &str,
    field: &str,
) -> MLResult<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(MLError::InvalidModel(format!(
            "{context}: {field} has length {actual}, expected {expected}"
        )))
    }
}

pub(super) fn require_finite(values: &[f64], context: &str, field: &str) -> MLResult<()> {
    if let Some((index, value)) = values
        .iter()
        .enumerate()
        .find(|(_, value)| !value.is_finite())
    {
        Err(MLError::InvalidModel(format!(
            "{context}: {field}[{index}] must be finite, got {value}"
        )))
    } else {
        Ok(())
    }
}

pub(super) fn checked_product(
    values: &[usize],
    context: &str,
    description: &str,
) -> MLResult<usize> {
    values.iter().try_fold(1usize, |product, value| {
        product.checked_mul(*value).ok_or_else(|| {
            MLError::InvalidModel(format!("{context}: {description} overflows usize"))
        })
    })
}

pub(super) fn validate_state(state: &ForwardState) -> StorageBackendResult<()> {
    if state.num_channels == 0 && !state.channel_map.is_empty() {
        return Err(StorageBackendError::Other(
            "deep-fusion layer produced zero channels".into(),
        ));
    }
    for (doc_id, values) in &state.channel_map {
        if values.len() != state.num_channels {
            return Err(StorageBackendError::Other(format!(
                "deep-fusion doc {doc_id} has {} channels, expected {}",
                values.len(),
                state.num_channels
            )));
        }
        if let Some((index, value)) = values
            .iter()
            .enumerate()
            .find(|(_, value)| !value.is_finite())
        {
            return Err(StorageBackendError::Other(format!(
                "deep-fusion doc {doc_id} channel {index} is non-finite: {value}"
            )));
        }
    }
    Ok(())
}
