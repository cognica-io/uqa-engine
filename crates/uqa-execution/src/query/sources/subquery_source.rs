//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for derived-table sources.

use super::streaming_subquery::try_build_streaming_subquery_with_sources;
/// Build the physical operator for a subquery source.
use super::{
    attach_closed_qualifier_filter, execute_view_plan_output_with_parent_cache,
    qualify_source_operator_with_columns, query_cte_names_with_control, ColumnPrune, CteScope,
    PhysicalOperator, QualifierFilters, QueryPlan, SQLError, SQLParam, SourceContext, SourcePlan,
};
use crate::query::binding::sources::SourceBindings;

pub(super) fn build_subquery_source_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    bindings: Option<&SourceBindings<'_>>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    match from {
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
            ..
        } => {
            // During a tuple-local recheck, a derived table named as the lock target pins every base scan of its storage inside this subtree to the candidate's tuples.
            let visible_qualifier = alias.clone().unwrap_or_default();
            let specialized = super::output_filter::specialize_source_filter(
                context,
                body,
                &visible_qualifier,
                column_aliases,
                filters,
            )?;
            let filters = if specialized.is_some() { None } else { filters };
            let body = specialized.as_deref().unwrap_or(body);
            let mut recheck_scope = ctes.enter_recheck_storage_pins(&visible_qualifier);
            let ctes: &mut CteScope<S> = &mut recheck_scope;
            if let Some(operator) =
                try_build_streaming_subquery_with_sources(context, body, params, ctes, bindings)?
            {
                if let SourcePlan::Subquery {
                    captured_output: Some(relation),
                    ..
                } = from
                {
                    return Ok(Box::new(crate::ColumnSelection::capturing_output(
                        operator, *relation,
                    )));
                }
                let source_columns = operator.schema().to_vec();
                let qualifier = alias.as_deref().unwrap_or_default();
                let operator = qualify_source_operator_with_columns(
                    operator,
                    &source_columns,
                    qualifier,
                    prune,
                    column_aliases,
                    ctes.lock_identities.emit,
                );
                return attach_closed_qualifier_filter(
                    operator, qualifier, filters, context, params, ctes,
                );
            }
            let operator = deferred_subquery_operator(context, body, params, ctes)?;
            if let SourcePlan::Subquery {
                captured_output: Some(relation),
                ..
            } = from
            {
                return Ok(Box::new(crate::ColumnSelection::capturing_output(
                    operator, *relation,
                )));
            }
            let source_columns = operator.schema().to_vec();
            let qualifier = alias.as_deref().unwrap_or_default();
            let operator = qualify_source_operator_with_columns(
                operator,
                &source_columns,
                qualifier,
                prune,
                column_aliases,
                ctes.lock_identities.emit,
            );
            attach_closed_qualifier_filter(operator, qualifier, filters, context, params, ctes)
        }
        _ => unreachable!("subquery source builder called for a different source kind"),
    }
}

fn deferred_subquery_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    body: &QueryPlan,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let schema = crate::query::binding::bind_query_plan_schema(
        context.ctes.routines,
        body,
        params,
        ctes,
        ctes.row_lock_outer_row().map(|row| &row.schema),
    )?;
    if let Some(operator) = context.ctes.queries.retained_query(body, params, ctes)? {
        let positions = schema
            .columns()
            .iter()
            .cloned()
            .enumerate()
            .map(|(position, name)| (name, position))
            .collect::<Vec<_>>();
        if !schema.columns_are_open(None) {
            return crate::ColumnSelection::with_bound_output(operator, &positions, schema)
                .map(|operator| Box::new(operator) as Box<dyn PhysicalOperator + 'a>)
                .map_err(crate::physical::physical_exec_error);
        }
        return Ok(operator);
    }
    let context_copy = *context;
    let resources = context.ctes.production_resources()?;
    let control = resources.control();
    let body =
        uqa_sql::plan::OwnedQueryPlan::from_produced(body.clone_with_control(&control)?, &control)?;
    let mut scope = ctes.copy_with_control(&control)?;
    let bound_output = (!schema.columns_are_open(None)).then_some(schema);
    let deferred_schema = if bound_output.is_some()
        && !super::output_descriptor::requires_physical_descriptor_with_control(
            &body, &scope, control,
        )? {
        bound_output.clone()
    } else {
        None
    };
    let callback_resources = resources.clone();
    let operator = crate::query::deferred::DeferredOperator::from_bound_schema_with_control(
        deferred_schema,
        move || {
            let local_cte_names =
                query_cte_names_with_control(&body, &callback_resources.control())?;
            let output = execute_view_plan_output_with_parent_cache(
                &context_copy,
                &body,
                params,
                &mut scope,
                &local_cte_names,
            )?;
            let columns = output
                .columns
                .iter()
                .cloned()
                .enumerate()
                .map(|(position, name)| (name, position))
                .collect::<Vec<_>>();
            let source = output.into_operator();
            let source = match bound_output {
                Some(schema) => crate::ColumnSelection::with_bound_output(source, &columns, schema)
                    .map_err(crate::physical::physical_exec_error)?,
                None => crate::ColumnSelection::compacting_with_positions(source, columns)
                    .preserving_lock_origins(),
            };
            Ok(Box::new(source))
        },
        &control,
    )?;
    Ok(operator)
}
