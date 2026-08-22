//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for derived-table sources.

use super::{
    attach_qualifier_filter, execute_view_plan_output_with_parent_cache,
    qualify_source_operator_with_columns, query_cte_names, try_build_streaming_subquery_operator,
    ColumnPrune, CteScope, Engine, QualifierFilters, SQLError, SQLParam, SourcePlan,
};
use uqa_execution::PhysicalOperator;

/// Build the physical operator for a subquery source.
pub(super) fn build_subquery_source_operator<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    match from {
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            // During a tuple-local recheck, a derived table named as the lock target pins every base scan of its storage inside this subtree to the candidate's tuples.
            let visible_qualifier = alias.clone().unwrap_or_default();
            let mut recheck_scope = ctes.enter_recheck_storage_pins(&visible_qualifier);
            let ctes: &mut CteScope = &mut recheck_scope;
            if let Some(operator) =
                try_build_streaming_subquery_operator(engine, body, params, ctes)?
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
                return Ok(attach_qualifier_filter(
                    operator, qualifier, filters, engine, params, ctes,
                ));
            }
            let local_cte_names = query_cte_names(body);
            let output = execute_view_plan_output_with_parent_cache(
                engine,
                body,
                params,
                ctes,
                &local_cte_names,
            )?;
            let source_columns = output.internal_columns.clone();
            let operator = output.into_operator();
            let qualifier = alias.as_deref().unwrap_or_default();
            let operator = qualify_source_operator_with_columns(
                operator,
                &source_columns,
                qualifier,
                prune,
                column_aliases,
                ctes.lock_identities.emit,
            );
            Ok(attach_qualifier_filter(
                operator, qualifier, filters, engine, params, ctes,
            ))
        }
        _ => unreachable!("subquery source builder called for a different source kind"),
    }
}
