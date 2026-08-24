//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Recursive physical operator assembly for FROM sources.

use super::{
    attach_qualifier_filter, build_function_group_source_operator, build_function_source_operator,
    build_join_source_operator, build_subquery_source_operator, build_table_source_operator,
    build_values_source_operator, qualifier_filter, ColumnPrune, CteScope, Engine,
    QualifierFilters, SQLError, SQLParam, SourcePlan,
};

/// Build a complete FROM source as a pull-based physical operator. Unlike the compatibility `build_join_rows_*` entry points below, this is the query executor's primary path and never collects a join, view, CTE, derived table, or table-function result into a cardinality-sized `Vec`.
pub(in crate::sql) fn build_join_operator_with_ctes<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    build_join_operator_with_ctes_at_path(engine, from, params, ctes, prune, filters, None)
}

pub(in crate::sql) fn build_join_operator_with_recheck_pins<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    build_join_operator_with_ctes_at_path(
        engine,
        from,
        params,
        ctes,
        prune,
        filters,
        Some(Vec::new()),
    )
}

#[allow(clippy::too_many_arguments)]
/// Recursively assemble a source tree while tracking tuple-recheck paths.
pub(super) fn build_join_operator_with_ctes_at_path<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    recheck_path: Option<Vec<u8>>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    use uqa_execution::PhysicalOperator;

    if let Some(source) = recheck_path
        .as_deref()
        .and_then(|path| ctes.recheck_source_row(path))
    {
        let scan: Box<dyn PhysicalOperator + 'a> = Box::new(
            uqa_execution::TableScan::from_physical_rows(source.schema, vec![source.row]),
        );
        if matches!(from, SourcePlan::Values { .. })
            || qualifier_filter(filters, &source.qualifier)
                .is_some_and(|predicate| uqa_planner::optimizer::contains_retrieval(&predicate))
        {
            return Ok(scan);
        }
        return Ok(attach_qualifier_filter(
            scan,
            &source.qualifier,
            filters,
            engine,
            params,
            ctes,
        ));
    }

    match from {
        SourcePlan::Table { .. } => {
            build_table_source_operator(engine, from, params, ctes, prune, filters)
        }
        SourcePlan::Join { .. } => {
            build_join_source_operator(engine, from, params, ctes, prune, filters, recheck_path)
        }
        SourcePlan::Values { .. } => {
            build_values_source_operator(engine, from, params, ctes, prune)
        }
        SourcePlan::Function { .. } => {
            build_function_source_operator(engine, from, params, ctes, prune, filters)
        }
        SourcePlan::FunctionGroup { .. } => {
            build_function_group_source_operator(engine, from, params, ctes, prune, filters)
        }
        SourcePlan::Subquery { .. } => {
            build_subquery_source_operator(engine, from, params, ctes, prune, filters)
        }
    }
}
