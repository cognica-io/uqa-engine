//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked conversion of graph property values to finite numeric weights.

use super::{GraphStoreError, GraphStoreResult, Value};

pub(super) fn value_as_f64(v: &Value) -> GraphStoreResult<Option<f64>> {
    match v {
        Value::Int(n) if (-9_007_199_254_740_992..=9_007_199_254_740_992).contains(n) => {
            Ok(Some(*n as f64))
        }
        Value::Int(n) => Err(GraphStoreError::InvalidMutation(format!(
            "integer {n} cannot be represented exactly as f64"
        ))),
        Value::Float(f) if f.is_finite() => Ok(Some(*f)),
        Value::Float(f) => Err(GraphStoreError::InvalidMutation(format!(
            "numeric graph property must be finite, got {f}"
        ))),
        Value::Decimal(value) => value.to_f64().map(Some).ok_or_else(|| {
            GraphStoreError::InvalidMutation(
                "decimal graph property cannot be represented as f64".into(),
            )
        }),
        Value::Bool(b) => Ok(Some(if *b { 1.0 } else { 0.0 })),
        _ => Ok(None),
    }
}
