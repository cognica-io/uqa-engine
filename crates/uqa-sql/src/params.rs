//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind parameters for `Engine::sql(query, params)`.

use uqa_core::Value;

/// Value bound to a `$N` placeholder.
#[derive(Debug, Clone)]
pub enum SQLParam {
    Scalar(Value),
    Vector(Vec<f32>),
}

impl SQLParam {
    pub fn scalar(value: Value) -> Self {
        Self::Scalar(value)
    }

    pub fn vector(v: Vec<f32>) -> Self {
        Self::Vector(v)
    }
}
