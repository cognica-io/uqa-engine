//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for derived-table sources.

/// Build the physical operator for a subquery source.
use super::{
    attach_closed_qualifier_filter, execute_view_plan_output_with_parent_cache,
    qualify_source_operator_with_columns, query_cte_names, try_build_streaming_subquery_operator,
    ColumnPrune, CteScope, PhysicalOperator, QualifierFilters, SQLError, SQLParam, SourceContext,
    SourcePlan,
};

pub(super) fn build_subquery_source_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
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
            let mut recheck_scope = ctes.enter_recheck_storage_pins(&visible_qualifier);
            let ctes: &mut CteScope<S> = &mut recheck_scope;
            if let Some(operator) =
                try_build_streaming_subquery_operator(context, body, params, ctes)?
            {
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
                return Ok(attach_closed_qualifier_filter(
                    operator, qualifier, filters, context, params, ctes,
                ));
            }
            let schema = crate::query::binding::bind_query_plan_schema(
                context.ctes.routines,
                body,
                params,
                ctes,
                ctes.row_lock_outer_row().map(|row| &row.schema),
            )?;
            let context_copy = *context;
            let body = body.clone();
            let mut scope = ctes.clone();
            let bound_output = (!schema.columns_are_open(None)).then_some(schema);
            let deferred_schema = if bound_output.is_some()
                && !super::output_descriptor::requires_physical_descriptor(&body, &scope)?
            {
                bound_output.clone()
            } else {
                None
            };
            let operator = crate::query::deferred::DeferredOperator::from_bound_schema(
                deferred_schema,
                Box::new(move || {
                    let local_cte_names = query_cte_names(&body);
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
                        Some(schema) => {
                            crate::ColumnSelection::with_bound_output(source, &columns, schema)
                                .map_err(crate::physical::physical_exec_error)?
                        }
                        None => crate::ColumnSelection::compacting_with_positions(source, columns)
                            .preserving_lock_origins(),
                    };
                    Ok(Box::new(source))
                }),
            )?;
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
            Ok(attach_closed_qualifier_filter(
                operator, qualifier, filters, context, params, ctes,
            ))
        }
        _ => unreachable!("subquery source builder called for a different source kind"),
    }
}
