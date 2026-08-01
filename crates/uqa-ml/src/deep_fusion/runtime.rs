//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked allocation, numeric bridges, logits, and gating.

use super::{
    logit, sigmoid, try_filled_vec, try_vec_with_capacity, Gating, StorageBackendError,
    StorageBackendResult, PROB_EPSILON,
};

pub(super) fn runtime_model_error(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

pub(super) fn runtime_vec_with_capacity<T>(
    capacity: usize,
    context: &str,
) -> StorageBackendResult<Vec<T>> {
    try_vec_with_capacity(capacity, context).map_err(|error| runtime_model_error(error.to_string()))
}

pub(super) fn runtime_filled_vec<T: Clone>(
    length: usize,
    value: T,
    context: &str,
) -> StorageBackendResult<Vec<T>> {
    try_filled_vec(length, value, context).map_err(|error| runtime_model_error(error.to_string()))
}

pub(super) fn usize_to_f64_exact(value: usize, context: &str) -> StorageBackendResult<f64> {
    const MAX_EXACT_INTEGER: u64 = 9_007_199_254_740_992;
    let value = u64::try_from(value)
        .map_err(|_| runtime_model_error(format!("{context} exceeds the u64 bridge")))?;
    if value > MAX_EXACT_INTEGER {
        return Err(runtime_model_error(format!(
            "{context} exceeds f64's exact integer range"
        )));
    }
    Ok(value as f64)
}

pub(super) fn safe_logit(p: f64) -> f64 {
    let clamped = p.clamp(PROB_EPSILON, 1.0 - PROB_EPSILON);
    logit(clamped)
}

pub(super) fn apply_gating(x: f64, gating: Gating) -> f64 {
    match gating {
        Gating::None => x,
        Gating::Softplus => {
            if x > 20.0 {
                x
            } else if x < -20.0 {
                x.exp()
            } else {
                x.exp().ln_1p()
            }
        }
        Gating::Sigmoid => sigmoid(x),
        Gating::ReLU => x.max(0.0),
        Gating::Swish => x * sigmoid(x),
        Gating::Gelu => {
            let scaled = (2.0 / std::f64::consts::PI).sqrt() * (x + 0.044_715 * x * x * x);
            0.5 * x * (1.0 + scaled.tanh())
        }
    }
}
