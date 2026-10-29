//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL assignment conversion and catalog-defined domain constraints.

use crate::expr::EngineHook;
use crate::{ast::Expr, catalog::domain::DomainCatalog, RowSchema, SQLError};
use uqa_core::Value;

/// Domain metadata and bound check evaluation required by assignment conversion.
pub trait AssignmentContext: EngineHook + DomainCatalog {
    fn evaluate_domain_check(
        &self,
        expression: &Expr,
        row: &dyn crate::expr::RowLookup,
        schema: &RowSchema,
    ) -> Result<Value, SQLError>;

    /// External adapters return the selected output spelling. Native catalogs admit visibility and name construction before producing it.
    fn domain_display_name_with_control(
        &self,
        oid: u32,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<String>, SQLError> {
        control.check()?;
        let name = self
            .resolve_regtype_output(&crate::ColumnType::Regtype, i64::from(oid))
            .map_err(SQLError::Internal)?
            .ok_or_else(|| SQLError::Internal("domain type has no catalog display name".into()))?;
        let (value, memory) = control
            .retain_external_value(Value::Str(name))?
            .into_parts();
        let Value::Str(name) = value else {
            unreachable!("domain output name")
        };
        Ok(control.finish(name, memory)?)
    }

    /// Native evaluation keeps lowering, binding and execution under this control; external callbacks own their construction and return an admitted result.
    fn evaluate_domain_check_with_control(
        &self,
        expression: &Expr,
        row: &dyn crate::expr::RowLookup,
        schema: &RowSchema,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Value>, SQLError> {
        control.check()?;
        Ok(control.retain_external_value(self.evaluate_domain_check(expression, row, schema)?)?)
    }
}

pub mod conversion;
pub mod domain;
pub mod vectors;

#[cfg(test)]
mod tests;

pub mod routines;

pub mod columns;
pub mod containers;
pub mod path;
pub mod targets;
