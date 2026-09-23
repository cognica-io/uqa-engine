//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind parameters for `Engine::sql(query, params)`.

use crate::SQLError;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

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

    /// Materialize the parameter carrier with ordinary SQL value semantics.
    pub fn to_value(&self) -> Result<Value, SQLError> {
        self.to_value_with_control(&ProductionControl::uncontrolled())
            .map(|value| value.into_uncontrolled().expect("ordinary parameter value"))
    }

    /// Copy a borrowed parameter while retaining every container and child payload under its producer's allowance.
    pub fn to_value_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        control.check()?;
        match self {
            Self::Scalar(value) | Self::TypedScalar { value, .. } => Ok(control.copy_value(value)?),
            Self::Vector(values) => vector_value(values, control),
            Self::Tensor(vectors) => {
                let mut output = ProductionVec::new(*control);
                output.reserve(vectors.len())?;
                for values in vectors {
                    output.push_produced(vector_value(values, control)?)?;
                }
                let (values, memory) = output.finish()?.into_parts();
                Ok(control.finish(Value::List(values), memory)?)
            }
        }
    }

    pub fn vector(v: Vec<f32>) -> Self {
        Self::Vector(v)
    }

    pub fn tensor(v: Vec<Vec<f32>>) -> Self {
        Self::Tensor(v)
    }
}

fn vector_value(
    values: &[f32],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let mut output = ProductionVec::new(*control);
    output.reserve(values.len())?;
    for value in values {
        output.push_produced(
            control.finish(Value::Float(f64::from(*value)), control.empty_reservation())?,
        )?;
    }
    let (values, memory) = output.finish()?.into_parts();
    Ok(control.finish(Value::List(values), memory)?)
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

    #[test]
    fn parameter_value_owners_keep_tensor_and_scalar_payloads_until_drop() {
        use uqa_core::{memory::MemoryBudget, CancellationToken};
        let budget = MemoryBudget::new(1 << 16);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        for (parameter, expected) in [
            (
                SQLParam::typed_scalar(Value::Str("payload".repeat(32)), ColumnType::Text),
                Value::Str("payload".repeat(32)),
            ),
            (
                SQLParam::Vector(vec![1.0, 2.5]),
                Value::List(vec![Value::Float(1.0), Value::Float(2.5)]),
            ),
            (
                SQLParam::Tensor(vec![vec![1.0, 2.5], vec![]]),
                Value::List(vec![
                    Value::List(vec![Value::Float(1.0), Value::Float(2.5)]),
                    Value::List(vec![]),
                ]),
            ),
        ] {
            let value = parameter.to_value_with_control(&control).unwrap();
            assert_eq!(*value, expected);
            assert!(value.reserved_bytes() > 0);
            assert_eq!(budget.used(), value.reserved_bytes());
            drop(value);
            assert_eq!(budget.used(), 0);
            assert_eq!(parameter.to_value().unwrap(), expected);
        }
    }

    #[test]
    fn parameter_production_releases_partial_output_on_quota_and_both_tokens() {
        use uqa_core::{memory::MemoryBudget, CancellationToken};
        let budget = MemoryBudget::new(256);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let held = control.copy_text("held").unwrap();
        let parameter = SQLParam::Tensor(vec![vec![1.0; 64], vec![2.0; 64]]);
        assert_eq!(
            parameter
                .to_value_with_control(&control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), held.reserved_bytes());
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                parameter
                    .to_value_with_control(&control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(budget.used(), held.reserved_bytes());
            token.reset();
        }
        assert_eq!(
            parameter.to_value().unwrap(),
            Value::List(vec![
                Value::List(vec![Value::Float(1.0); 64]),
                Value::List(vec![Value::Float(2.0); 64])
            ])
        );
    }
}
