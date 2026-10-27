//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source filtering and scoped child-query cache restoration.

use super::{
    physical_exec_error, qualifier_filter, BTreeSet, CteScope, JoinKind, PhysicalOperator,
    QualifierFilters, QueryOutput, QueryPlan, RowSchema, SQLError, SQLParam, ScalarExpr,
    SourceContext, Value,
};
use uqa_sql::ResultRow;

pub(super) fn execute_view_plan_output_with_parent_cache<S: Clone + 'static>(
    context: &SourceContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    local_cte_names: &BTreeSet<String>,
) -> Result<QueryOutput, SQLError> {
    let saved = crate::query::scope::bindings::save_cte_names(ctes, local_cte_names);
    let result = if let Some(outer) = ctes.row_lock_outer_row() {
        context
            .ctes
            .queries
            .execute_lateral_query(plan, outer, params, ctes)
    } else {
        context.ctes.queries.execute_query(plan, params, ctes)
    };
    crate::query::scope::bindings::restore_cte_names(ctes, saved);
    result
}

pub(super) fn attach_qualifier_filter<'a, S: Clone + 'static>(
    operator: Box<dyn PhysicalOperator + 'a>,
    qualifier: &str,
    filters: Option<&QualifierFilters>,
    context: &SourceContext<'a, S>,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
) -> Box<dyn PhysicalOperator + 'a> {
    let Some(predicate) = qualifier_filter(filters, qualifier) else {
        return operator;
    };
    Box::new(crate::Filter::with_evaluator(
        operator,
        predicate,
        context.relational.evaluator(params, ctes),
    ))
}

/// Attach the predicates placed on a source whose row carries every column of its qualifier; see [`filter_closed_source`].
pub(super) fn attach_closed_qualifier_filter<'a, S: Clone + 'static>(
    operator: Box<dyn PhysicalOperator + 'a>,
    qualifier: &str,
    filters: Option<&QualifierFilters>,
    context: &SourceContext<'a, S>,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
) -> Box<dyn PhysicalOperator + 'a> {
    filter_closed_source(operator, qualifier, filters, || {
        context.relational.evaluator(params, ctes)
    })
}

/// Apply the predicates placed on a source whose row carries every column of its qualifier, such as a VALUES list, a table function or a derived table. A placed predicate that reads another column of that qualifier would read NULL for every row, so the filter fails with an internal error when it receives a row instead.
pub(super) fn filter_closed_source<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
    qualifier: &str,
    filters: Option<&QualifierFilters>,
    evaluator: impl FnOnce() -> crate::SharedExpressionEvaluator<'a>,
) -> Box<dyn PhysicalOperator + 'a> {
    let Some(predicate) = qualifier_filter(filters, qualifier) else {
        return operator;
    };
    if let Some(column) = unproduced_column(&predicate, qualifier, operator.row_schema()) {
        return Box::new(crate::Filter::unresolved(operator, column));
    }
    Box::new(crate::Filter::with_evaluator(
        operator,
        predicate,
        evaluator(),
    ))
}

/// The first `qualifier.column` reference of `predicate` that `schema` does not produce.
pub(super) fn unproduced_column(
    predicate: &ScalarExpr,
    qualifier: &str,
    schema: &RowSchema,
) -> Option<String> {
    let mut unproduced = None;
    predicate.visit(&mut |node| {
        if let ScalarExpr::QualifiedColumn {
            qualifier: reference,
            column,
        } = node
        {
            if unproduced.is_none()
                && reference == qualifier
                && schema.qualified_position(reference, column).is_none()
                && schema.qualified_slot(reference, column).is_none()
                && !schema.qualified_column_is_ambiguous(reference, column)
            {
                unproduced = Some(format!("{reference}.{column}"));
            }
        }
    });
    unproduced
}

pub(super) fn null_row_for_schema(schema: &[String]) -> ResultRow {
    schema
        .iter()
        .map(|column| (column.clone(), Value::Null))
        .collect()
}

pub(super) fn shape_join_using_output<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: &uqa_sql::semantics::ResolvedJoinUsing,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let (operator, (columns, aliases)) =
        crate::physical::construction::OperatorBuild::run(operator, |_| {
            uqa_sql::semantics::join_using_layout(kind, left, right, using)
        })?;
    crate::JoinOutput::try_new(operator, columns, aliases)
        .map(|output| Box::new(output) as Box<dyn PhysicalOperator + 'a>)
        .map_err(physical_exec_error)
}
