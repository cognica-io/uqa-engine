//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validation and lowering of deep-fusion layer specifications.

use super::{DriverResult, GatingSpec, SQLError};

pub(super) fn lower_deep_conv(
    edge_label: Option<&str>,
    hop_weights: &[f64],
    direction: uqa_operators::DeepGraphDirection,
) -> DriverResult<uqa_ml::Layer> {
    if hop_weights.is_empty()
        || hop_weights
            .iter()
            .any(|weight| !weight.is_finite() || *weight < 0.0)
        || hop_weights.iter().sum::<f64>() <= 0.0
    {
        return Err(SQLError::TypeMismatch(format!(
            "DeepFusion Conv.hop_weights must be a non-empty finite non-negative vector with positive sum, got {hop_weights:?}"
        )));
    }
    Ok(uqa_ml::Layer::Conv {
        edge_label: edge_label.unwrap_or_default().to_string(),
        hop_weights: hop_weights.to_vec(),
        direction,
    })
}

pub(super) fn lower_deep_pool(
    edge_label: Option<&str>,
    pool_size: usize,
    method: uqa_operators::DeepFusionPoolMethod,
    direction: uqa_operators::DeepGraphDirection,
) -> DriverResult<uqa_ml::Layer> {
    if pool_size == 0 {
        return Err(SQLError::TypeMismatch(
            "DeepFusion Pool.pool_size must be positive".to_string(),
        ));
    }
    let method = match method {
        uqa_operators::DeepFusionPoolMethod::Average => uqa_ml::DeepPoolMethod::Avg,
        uqa_operators::DeepFusionPoolMethod::Max => uqa_ml::DeepPoolMethod::Max,
    };
    Ok(uqa_ml::Layer::Pool {
        edge_label: edge_label.unwrap_or_default().to_string(),
        pool_size,
        method,
        direction,
    })
}

pub(super) fn lower_deep_dense(
    weights: &[f64],
    bias: &[f64],
    output_channels: usize,
    input_channels: usize,
) -> DriverResult<uqa_ml::Layer> {
    let Some(expected_weights) = output_channels.checked_mul(input_channels) else {
        return Err(SQLError::TypeMismatch(
            "DeepFusion Dense dimensions overflow usize".to_string(),
        ));
    };
    if output_channels == 0
        || input_channels == 0
        || weights.len() != expected_weights
        || bias.len() != output_channels
        || weights.iter().chain(bias).any(|value| !value.is_finite())
    {
        return Err(SQLError::TypeMismatch(format!(
            "DeepFusion Dense requires positive dimensions, {expected_weights} weights, and {output_channels} biases; got {} weights and {} biases",
            weights.len(),
            bias.len()
        )));
    }
    Ok(uqa_ml::Layer::Dense {
        weights: weights.to_vec(),
        bias: bias.to_vec(),
        output_channels,
        input_channels,
    })
}

pub(super) fn lower_deep_batch_norm(epsilon: f64) -> DriverResult<uqa_ml::Layer> {
    if !epsilon.is_finite() || epsilon <= 0.0 {
        return Err(SQLError::TypeMismatch(format!(
            "DeepFusion BatchNorm.epsilon must be finite and positive, got {epsilon}"
        )));
    }
    Ok(uqa_ml::Layer::BatchNorm { epsilon })
}

pub(super) fn lower_deep_dropout(probability: f64) -> DriverResult<uqa_ml::Layer> {
    if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
        return Err(SQLError::TypeMismatch(format!(
            "DeepFusion Dropout.probability must be finite and in [0, 1], got {probability}"
        )));
    }
    Ok(uqa_ml::Layer::Dropout { p: probability })
}

pub(super) fn deep_runtime_gating(gating: &GatingSpec) -> uqa_ml::Gating {
    match gating {
        GatingSpec::Softplus => uqa_ml::Gating::Softplus,
        GatingSpec::Pass => uqa_ml::Gating::None,
        GatingSpec::Sigmoid { .. } => uqa_ml::Gating::Sigmoid,
        GatingSpec::ReLU => uqa_ml::Gating::ReLU,
        GatingSpec::Swish => uqa_ml::Gating::Swish,
        GatingSpec::Gelu => uqa_ml::Gating::Gelu,
    }
}
