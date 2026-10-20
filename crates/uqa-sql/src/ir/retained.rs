//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable evaluated constants share storage without retaining every decoded payload.

use std::{fmt, sync::Arc};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uqa_core::Value;

use crate::{ast::ColumnType, SQLError};

/// Storage for an already evaluated value. Reading must reproduce the original value without invoking SQL or changing it; implementations own their resource until the final shared handle is dropped.
pub trait ConstantValueSource: fmt::Debug + Send + Sync {
    fn read(&self, index: u64) -> Result<Value, SQLError>;
}

/// An immutable positional value in a shared owner. Copying this handle never reads or copies the payload.
#[derive(Clone)]
pub struct RetainedConstantValue {
    source: Arc<dyn ConstantValueSource>,
    index: u64,
}

impl RetainedConstantValue {
    pub fn new(source: Arc<dyn ConstantValueSource>, index: u64) -> Self {
        Self { source, index }
    }

    pub fn read(&self) -> Result<Value, SQLError> {
        crate::expr::check_statement_cancellation()?;
        let value = self.source.read(self.index)?;
        crate::expr::check_statement_cancellation()?;
        Ok(value)
    }
}

impl fmt::Debug for RetainedConstantValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedConstantValue")
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

impl PartialEq for RetainedConstantValue {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.source, &other.source) && self.index == other.index {
            return true;
        }
        // IR representation comparison may conservatively decline a rewrite when a resource cannot be read. SQL evaluation retains the original fallible read and reports its error.
        match (self.read(), other.read()) {
            (Ok(left), Ok(right)) => left.has_same_representation(&right),
            _ => false,
        }
    }
}

impl Serialize for RetainedConstantValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.read()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RetainedConstantValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(Self::new(Arc::new(OwnedConstant(value)), 0))
    }
}

#[derive(Debug)]
struct OwnedConstant(Value);

impl ConstantValueSource for OwnedConstant {
    fn read(&self, index: u64) -> Result<Value, SQLError> {
        if index != 0 {
            return Err(SQLError::Internal(
                "retained constant index is outside its owner".into(),
            ));
        }
        Ok(self.0.clone())
    }
}

/// Selected result metadata of a constant whose bytes are owned by execution storage. This carries an evaluated datum, never an expression to evaluate again.
#[derive(Debug, Clone, PartialEq)]
pub struct RetainedScalarLiteral {
    pub value: RetainedConstantValue,
    pub ty: ColumnType,
    pub declaration: Option<String>,
    pub collation_oid: Option<u32>,
}

impl Serialize for RetainedScalarLiteral {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.materialize()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl RetainedScalarLiteral {
    /// Materialize one value only at an interface that requires an owned literal. Cloning plans retains the shared owner instead.
    pub fn materialize(&self) -> Result<crate::ScalarExpr, SQLError> {
        let value = self.value.read()?;
        let Some(declaration) = &self.declaration else {
            return Ok(crate::ScalarExpr::Literal(value));
        };
        Ok(crate::ScalarExpr::TypedLiteral {
            value,
            ty: declaration.clone(),
            bound_type: Some(self.ty.clone()),
            parameter_index: None,
            composite_source: None,
            collation_oid: self.collation_oid,
        })
    }
}

/// Planner supplies a successfully evaluated constant; execution supplies its bounded storage implementation.
pub trait ConstantValueStorage: fmt::Debug + Send + Sync {
    fn retain(&self, value: Value) -> Result<RetainedConstantValue, SQLError>;
}

#[cfg(test)]
mod tests;
