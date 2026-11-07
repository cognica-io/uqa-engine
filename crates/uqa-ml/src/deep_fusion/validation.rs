//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Model shape, weight, recurrent, and runtime-state validation.

use super::{ForwardState, MLError, MLResult, StorageBackendError, StorageBackendResult};

mod input;
use input::ValidationLayer as Layer;

pub(super) fn validate_layers(layers: &[super::Layer], alpha: f64) -> MLResult<()> {
    ModelValidation::new(Inputs::Runtime(layers.iter()), alpha)?.finish()
}

pub(crate) fn validate_model(layers: &[crate::DeepLayerSpec], alpha: f64) -> MLResult<()> {
    ModelValidation::model(layers, alpha)?.finish()
}

mod cursor;
pub(crate) use cursor::ModelValidation;
use cursor::{Inputs, LayerContext, Numbers};

fn validate_layer<'a>(index: usize, layer: &Layer<'a>, numbers: &mut Numbers<'a>) -> MLResult<()> {
    let context = LayerContext(index);
    match layer {
        Layer::Input { dimensions } => validate_input(index, *dimensions, &context),
        Layer::Signal(signals) if *signals == 0 => Err(MLError::InvalidModel(format!(
            "{context}: Signal requires at least one operator"
        ))),
        Layer::Embed(values) => validate_embedding(index, values, &context, numbers),
        Layer::Dense {
            weights,
            bias,
            output_channels,
            input_channels,
        } => validate_dense(
            weights,
            bias,
            *output_channels,
            *input_channels,
            &context,
            numbers,
        ),
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
        Layer::CNN1D { .. } => validate_cnn_1d(layer, &context, numbers),
        Layer::CNN2D { .. } => validate_cnn_2d(layer, &context, numbers),
        Layer::Conv { hop_weights, .. } => validate_hop_weights(hop_weights, &context, numbers),
        Layer::Pool { pool_size, .. } => require_nonzero(*pool_size, &context, "pool_size"),
        Layer::Recurrent { .. } => validate_recurrent_spec(layer, 1, &context, numbers),
        Layer::LongShortTermMemory { .. } => validate_recurrent_spec(layer, 4, &context, numbers),
        Layer::Signal(_) | Layer::BatchNorm { .. } | Layer::Dropout { .. } | Layer::Other => Ok(()),
    }
}

pub(super) fn validate_input(
    index: usize,
    dimensions: usize,
    context: &(impl std::fmt::Display + ?Sized),
) -> MLResult<()> {
    if index != 0 {
        return Err(MLError::InvalidModel(format!(
            "{context}: Input is only valid as the first layer"
        )));
    }
    require_nonzero(dimensions, context, "dimensions")
}

fn validate_embedding<'a>(
    index: usize,
    values: &'a [f64],
    context: &(impl std::fmt::Display + ?Sized),
    numbers: &mut Numbers<'a>,
) -> MLResult<()> {
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
    numbers.push(values, "embedding");
    Ok(())
}

fn validate_dense<'a>(
    weights: &'a [f64],
    bias: &'a [f64],
    output_channels: usize,
    input_channels: usize,
    context: &(impl std::fmt::Display + ?Sized),
    numbers: &mut Numbers<'a>,
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
    numbers.push(weights, "weights");
    numbers.push(bias, "bias");
    Ok(())
}

fn validate_cnn_1d<'a>(
    layer: &Layer<'a>,
    context: &(impl std::fmt::Display + ?Sized),
    numbers: &mut Numbers<'a>,
) -> MLResult<()> {
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
    validate_weights_and_bias(weights, bias, expected, *output_channels, context, numbers)
}

fn validate_cnn_2d<'a>(
    layer: &Layer<'a>,
    context: &(impl std::fmt::Display + ?Sized),
    numbers: &mut Numbers<'a>,
) -> MLResult<()> {
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
    validate_weights_and_bias(weights, bias, expected, *output_channels, context, numbers)
}

fn validate_weights_and_bias<'a>(
    weights: &'a [f64],
    bias: &'a [f64],
    expected_weights: usize,
    output_channels: usize,
    context: &(impl std::fmt::Display + ?Sized),
    numbers: &mut Numbers<'a>,
) -> MLResult<()> {
    require_len(weights.len(), expected_weights, context, "weights")?;
    require_len(bias.len(), output_channels, context, "bias")?;
    numbers.push(weights, "weights");
    numbers.push(bias, "bias");
    Ok(())
}

fn validate_hop_weights<'a>(
    hop_weights: &'a [f64],
    context: &(impl std::fmt::Display + ?Sized),
    numbers: &mut Numbers<'a>,
) -> MLResult<()> {
    if hop_weights.is_empty() {
        return Err(MLError::InvalidModel(format!(
            "{context}: graph convolution requires at least one hop weight"
        )));
    }
    numbers.push(hop_weights, "hop_weights");
    numbers.hops = true;
    Ok(())
}

fn validate_recurrent_spec<'a>(
    layer: &Layer<'a>,
    gates: usize,
    context: &(impl std::fmt::Display + ?Sized),
    numbers: &mut Numbers<'a>,
) -> MLResult<()> {
    let (weights_input, weights_hidden, bias, hidden_channels, input_channels) = match layer {
        Layer::Recurrent {
            weights_input,
            weights_hidden,
            bias,
            hidden_channels,
            input_channels,
            ..
        }
        | Layer::LongShortTermMemory {
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
    numbers.push(weights_input, "weights_input");
    numbers.push(weights_hidden, "weights_hidden");
    numbers.push(bias, "bias");
    Ok(())
}

pub(super) fn require_nonzero(
    value: usize,
    context: &(impl std::fmt::Display + ?Sized),
    field: &str,
) -> MLResult<()> {
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
    context: &(impl std::fmt::Display + ?Sized),
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

pub(super) fn checked_product(
    values: &[usize],
    context: &(impl std::fmt::Display + ?Sized),
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
