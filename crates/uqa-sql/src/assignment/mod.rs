//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL assignment conversion and catalog-defined domain constraints.

use crate::expr::EngineHook;
use crate::{ast::Expr, catalog::domain::DomainCatalog, ResultRow, RowSchema, SQLError};
use uqa_core::Value;

/// Domain metadata and bound check evaluation required by assignment conversion.
pub trait AssignmentContext: EngineHook + DomainCatalog {
    fn evaluate_domain_check(
        &self,
        expression: &Expr,
        row: &ResultRow,
        schema: &RowSchema,
    ) -> Result<Value, SQLError>;
}

pub mod conversion;
pub mod domain;
pub mod vectors;

#[cfg(test)]
mod tests;

pub mod routines;

pub mod columns;
