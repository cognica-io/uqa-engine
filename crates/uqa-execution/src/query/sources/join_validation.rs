//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JOIN predicates bind to the input descriptors and retain their lexical outer values.

use super::{CteScope, RowSchema, SQLError, SQLParam, ScalarExpr, SourceContext};
use uqa_sql::plan::{MutableExpressionPlan, OwnedScalarExpr};

pub(super) fn bind_join_on_schema<'a, S: Clone + 'static>(
    context: &SourceContext<'a, S>,
    on: Option<&ScalarExpr>,
    left: &RowSchema,
    right: &RowSchema,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
) -> Result<
    (
        Option<OwnedScalarExpr>,
        crate::SharedExpressionEvaluator<'a>,
    ),
    SQLError,
> {
    let resources = context.ctes.production_resources()?;
    let control = resources.control();
    let Some(input) = on else {
        return Ok((
            None,
            crate::query::expression::with_outer_row_with_control(
                context.relational.evaluator(params, ctes)?,
                ctes.outer_row_binding(),
                &control,
            )?,
        ));
    };
    let schema = RowSchema::join_with_control(left, right, [], &control)?;
    let on;
    let mut rewritten_scope;
    let mut arena_scope;
    let ctes = if let Some(rewrites) = &ctes.source_outer_rewrites {
        let expression =
            MutableExpressionPlan::copy_from(input, &ctes.scalar_subqueries, &control)?;
        let expression = uqa_sql::semantics::view_rewrite::resolve_outer_expression_rewrites_owned(
            expression, rewrites, &schema, &control,
        )?;
        let (scalar, arena) = expression.into_parts(&control)?;
        on = scalar;
        let arena = arena.into_shared(&control)?;
        rewritten_scope = ctes.copy_with_control(&control)?;
        arena_scope = rewritten_scope.enter_retained_scalar_subqueries(arena);
        &mut *arena_scope
    } else {
        on = input.copy_for_rewrite(&control)?;
        ctes
    };
    let evaluator = crate::query::expression::with_outer_row_with_control(
        context.relational.evaluator(params, ctes)?,
        ctes.outer_row_binding(),
        &control,
    )?;
    evaluator.expression_type_with_control(&on, &schema, &control)?;
    Ok((
        Some(evaluator.bind_type_introspection_owned(on, &schema, &control)?),
        evaluator,
    ))
}
