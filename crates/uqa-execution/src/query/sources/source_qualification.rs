//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source filtering and scoped child-query cache restoration.

use super::{
    physical_exec_error, qualifier_filter, BTreeSet, CteScope, JoinKind, PhysicalOperator,
    QualifierFilters, QueryOutput, QueryPlan, RowSchema, SQLError, SQLParam, SourceContext, Value,
};
use uqa_sql::ResultRow;

pub(super) fn execute_view_plan_output_with_parent_cache<S: Clone + 'static>(
    context: &SourceContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    local_cte_names: &BTreeSet<String>,
) -> Result<QueryOutput, SQLError> {
    let saved = crate::query::scope::bindings::save_and_remove_cte_names(ctes, local_cte_names);
    let result = context.ctes.queries.execute_query(plan, params, ctes);
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
    let (columns, aliases) = uqa_sql::semantics::join_using_layout(kind, left, right, using)?;
    crate::JoinOutput::try_new(operator, columns, aliases)
        .map(|output| Box::new(output) as Box<dyn PhysicalOperator + 'a>)
        .map_err(physical_exec_error)
}
