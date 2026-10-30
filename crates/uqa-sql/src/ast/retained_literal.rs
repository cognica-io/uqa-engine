//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Frozen runtime input payloads preserve ordinary AST serialization and scalar lowering.

use super::Expr;
use crate::{ir::RetainedScalarLiteral, SQLError};
use serde::{Serialize, Serializer};

#[derive(Debug, Clone, PartialEq)]
pub struct RetainedInputLiteral(pub RetainedScalarLiteral);

impl RetainedInputLiteral {
    pub fn materialize(&self) -> Result<Expr, SQLError> {
        let value = self.0.value.read()?;
        Ok(match &self.0.declaration {
            Some(ty) => Expr::TypedLiteral {
                value,
                ty: ty.clone(),
                collation_oid: self.0.collation_oid,
                composite_source: None,
            },
            None => Expr::Literal(value),
        })
    }
}

impl Serialize for RetainedInputLiteral {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.materialize()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

#[cfg(test)]
mod tests;
