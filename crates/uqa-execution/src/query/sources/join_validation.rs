//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JOIN predicates bind to the input descriptors and retain their lexical outer values.

use super::{CteScope, RowSchema, SQLError, SQLParam, ScalarExpr, SourceContext};

pub(super) fn bind_join_on_schema<'a, S: Clone + 'static>(
    context: &SourceContext<'a, S>,
    on: Option<&ScalarExpr>,
    left: &RowSchema,
    right: &RowSchema,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
) -> Result<(Option<ScalarExpr>, crate::SharedExpressionEvaluator<'a>), SQLError> {
    let Some(on) = on else {
        return Ok((
            None,
            crate::query::expression::with_outer_row(
                context.relational.evaluator(params, ctes),
                ctes.row_lock_outer_row(),
            ),
        ));
    };
    let schema = RowSchema::join(left, right, std::iter::empty::<String>());
    let mut on = on.clone();
    let mut rewritten_scope;
    let mut arena_scope;
    let ctes = if let Some(rewrites) = &ctes.source_outer_rewrites {
        let mut subqueries = ctes.scalar_subqueries.to_vec();
        uqa_sql::semantics::view_rewrite::resolve_outer_expression_rewrites(
            &mut on,
            rewrites,
            &schema,
            &mut subqueries,
        );
        rewritten_scope = ctes.clone();
        arena_scope = rewritten_scope.enter_scalar_subqueries(&subqueries);
        &mut *arena_scope
    } else {
        ctes
    };
    let evaluator = crate::query::expression::with_outer_row(
        context.relational.evaluator(params, ctes),
        ctes.row_lock_outer_row(),
    );
    evaluator.expression_type(&on, &schema)?;
    Ok((
        Some(evaluator.bind_type_introspection(on, &schema)),
        evaluator,
    ))
}
