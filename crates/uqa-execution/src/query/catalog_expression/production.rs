//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog scalar preparation and evaluation retain their original producer owners.

use super::CatalogRowView;
use crate::{query::set_projection::SetFunctionRuntime, scalar::ScalarEvalContext, RowSchema};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::{expr::RowLookup, plan::OwnedScalarExpr, SQLError, SQLParam};

/// Evaluate an owned scalar through the ordinary inference, binding and evaluation order. The caller supplies Planner's rewrite operation and retains the statement scope; no expression, schema or row payload is cloned at this adapter boundary.
pub fn evaluate_catalog_scalar_with_control(
    expression: OwnedScalarExpr,
    row: &dyn RowLookup,
    schema: &RowSchema,
    params: &[SQLParam],
    hook: &dyn SetFunctionRuntime,
    control: &ProductionControl<'_>,
    optimize: impl FnOnce(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    let _source = uqa_sql::type_resolution::scalar_type_with_resolver_and_control(
        &expression,
        schema,
        params,
        Some(hook),
        control,
    )?;
    let expression = uqa_sql::type_resolution::bind_type_introspection_owned(
        expression,
        schema,
        params,
        Some(hook),
        control,
    )?;
    let expression = optimize(expression)?;
    let row = CatalogRowView { schema, row };
    let context = ScalarEvalContext::from_row_lookup(&row, params)
        .with_row_schema(schema)
        .with_function_hook(hook);
    crate::scalar::eval_scalar_with_control(&expression, &context, control)
}

#[cfg(test)]
mod tests;
