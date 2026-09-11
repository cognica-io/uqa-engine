//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture catalog binding scopes for stored statements and scalar expressions.

use crate::Engine;
use uqa_sql::{
    binding::stored_routines::BoundStatementRoutines, plan::UnifiedPlan, SQLError, SQLParam,
};

pub(crate) fn bind_catalog_statement_routines(
    engine: &Engine,
    plan: &UnifiedPlan,
) -> Result<BoundStatementRoutines, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    let binding = uqa_execution::query::binding::binding_context(&scope)?;
    uqa_sql::binding::stored_routines::bind_catalog_statement_routines(
        &uqa_sql::binding::stored_routines::CatalogRoutineContext {
            routines: engine,
            binding: &binding,
        },
        plan,
    )
}

/// Bind a catalog-owned scalar expression, including all nested query plans, against a statically typed outer row.
pub(crate) fn bind_catalog_expression_routines_with_outer(
    engine: &Engine,
    expression: &mut uqa_planner::ExpressionPlan,
    params: &[SQLParam],
    outer: &uqa_execution::RowSchema,
) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
    let ctes = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    uqa_execution::query::binding::bind_expression_plan_routines_for_storage(
        engine, expression, params, &ctes, outer,
    )
}
