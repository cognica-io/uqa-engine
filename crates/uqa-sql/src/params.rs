//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind parameters for `Engine::sql(query, params)`.

use uqa_core::Value;

use crate::ast::ColumnType;

/// Value bound to a `$N` placeholder.
#[derive(Debug, Clone)]
pub enum SQLParam {
    Scalar(Value),
    /// A scalar value whose declared SQL type must survive runtime [`Value`] carrier normalization.
    TypedScalar {
        value: Value,
        ty: ColumnType,
    },
    Vector(Vec<f32>),
    Tensor(Vec<Vec<f32>>),
}

impl SQLParam {
    pub fn scalar(value: Value) -> Self {
        Self::Scalar(value)
    }

    #[must_use]
    pub fn typed_scalar(value: Value, ty: ColumnType) -> Self {
        Self::TypedScalar { value, ty }
    }

    /// Return the scalar carrier without changing the semantics of untyped [`SQLParam::Scalar`] values.
    #[must_use]
    pub fn scalar_value(&self) -> Option<&Value> {
        match self {
            Self::Scalar(value) | Self::TypedScalar { value, .. } => Some(value),
            Self::Vector(_) | Self::Tensor(_) => None,
        }
    }

    /// Return the explicit SQL type carried only by [`SQLParam::TypedScalar`].
    #[must_use]
    pub fn declared_scalar_type(&self) -> Option<&ColumnType> {
        match self {
            Self::TypedScalar { ty, .. } => Some(ty),
            Self::Scalar(_) | Self::Vector(_) | Self::Tensor(_) => None,
        }
    }

    pub fn vector(v: Vec<f32>) -> Self {
        Self::Vector(v)
    }

    pub fn tensor(v: Vec<Vec<f32>>) -> Self {
        Self::Tensor(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_scalar_preserves_declared_type_without_changing_scalar_access() {
        let value = Value::Int(7);
        let typed = SQLParam::typed_scalar(value.clone(), ColumnType::SmallInteger);
        assert_eq!(typed.scalar_value(), Some(&value));
        assert_eq!(
            typed.declared_scalar_type(),
            Some(&ColumnType::SmallInteger)
        );

        let scalar = SQLParam::scalar(value.clone());
        assert_eq!(scalar.scalar_value(), Some(&value));
        assert_eq!(scalar.declared_scalar_type(), None);
    }
}
