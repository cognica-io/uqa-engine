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
    pub(crate) fn retain(
        expression: Expr,
        storage: &dyn crate::ir::ConstantValueStorage,
    ) -> Result<Expr, SQLError> {
        let (value, declaration) = match &expression {
            Expr::TypedLiteral {
                value,
                ty,
                composite_source: None,
                ..
            } => (value, Some(ty)),
            Expr::Literal(value) => (value, None),
            _ => return Ok(expression),
        };
        let cancellation = crate::expr::statement_cancellation().unwrap_or_default();
        let bytes = value.retained_payload_bytes(
            &uqa_core::memory::MemoryBudget::new(usize::MAX),
            &cancellation,
        )?;
        if bytes <= size_of::<crate::ScalarExpr>()
            || crate::expr::composites::literal::contains_records(value)
            || crate::expr::datums::contains_datum(value)
        {
            return Ok(expression);
        }
        let ty = if let Some(name) = declaration {
            crate::type_resolution::pg_catalog_builtin_type(name)
        } else {
            crate::type_resolution::scalar_type(
                &crate::ScalarExpr::Literal(value.clone()),
                &crate::RowSchema::default(),
                &[],
            )?
        };
        let Some(ty) = ty else {
            return Ok(expression);
        };
        if crate::type_resolution::pg_catalog_builtin_type(&ty.catalog_name()).as_ref() != Some(&ty)
        {
            return Ok(expression);
        }
        let (value, declaration, collation_oid) = match expression {
            Expr::TypedLiteral {
                value,
                ty,
                collation_oid,
                ..
            } => (value, Some(ty), collation_oid),
            Expr::Literal(value) => (value, None, None),
            _ => unreachable!("selected evaluated input"),
        };
        Ok(Expr::RetainedLiteral(Box::new(Self(
            RetainedScalarLiteral {
                value: storage.retain(value)?,
                ty,
                declaration,
                collation_oid,
            },
        ))))
    }

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
