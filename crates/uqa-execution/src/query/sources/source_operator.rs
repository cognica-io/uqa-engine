//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Recursive physical operator assembly for FROM sources.

/// Build a complete FROM source as a pull-based physical operator. Unlike the compatibility `build_join_rows_*` entry points below, this is the query executor's primary path and never collects a join, view, CTE, derived table, or table-function result into a cardinality-sized `Vec`.
use super::{
    attach_qualifier_filter, build_function_group_source_operator, build_function_source_operator,
    build_join_source_operator, build_subquery_source_operator, build_table_source_operator,
    build_values_source_operator, qualifier_filter, ColumnPrune, CteScope, QualifierFilters,
    SQLError, SQLParam, SourceContext, SourcePlan,
};
use crate::query::binding::sources::SourceBindings;

pub fn build_join_operator_with_ctes<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    build_join_operator_with_ctes_at_path(context, from, params, ctes, prune, filters, None, None)
}

pub(crate) fn build_join_operator_with_bound_sources<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    bindings: Option<&SourceBindings<'_>>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    build_join_operator_with_ctes_at_path(
        context, from, params, ctes, prune, filters, None, bindings,
    )
}

pub fn build_join_operator_with_recheck_pins<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    build_join_operator_with_ctes_at_path(
        context,
        from,
        params,
        ctes,
        prune,
        filters,
        Some(Vec::new()),
        None,
    )
}

/// Recursively assemble a source tree while tracking tuple-recheck paths.
#[expect(
    clippy::too_many_arguments,
    reason = "keeps source metadata and tuple recheck paths scoped to physical assembly"
)]
pub(super) fn build_join_operator_with_ctes_at_path<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    recheck_path: Option<Vec<u8>>,
    bindings: Option<&SourceBindings<'_>>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    let input = (|| {
        use crate::PhysicalOperator;
        let bindings = bindings.filter(|binding| binding.matches_source(from));

        if context.planning.source_is_empty(from) {
            return empty_source_operator(context, from, params, ctes, None);
        }

        if let Some(source) = recheck_path
            .as_deref()
            .and_then(|path| ctes.recheck_source_row(path))
        {
            let scan: Box<dyn PhysicalOperator + 'a> = Box::new(
                crate::TableScan::from_physical_rows(source.schema, vec![source.row]),
            );
            if matches!(
                from,
                SourcePlan::Values { .. } | SourcePlan::RetainedRows(_)
            ) || qualifier_filter(filters, &source.qualifier)
                .is_some_and(|predicate| uqa_sql::semantics::contains_retrieval(&predicate))
            {
                return Ok(scan);
            }
            return Ok(attach_qualifier_filter(
                scan,
                &source.qualifier,
                filters,
                context,
                params,
                ctes,
            ));
        }

        match from {
            SourcePlan::RetainedRows(source) => Ok(Box::new(
                super::retained_source::build_retained_source_operator(source)?,
            )),
            SourcePlan::Table { .. } => {
                build_table_source_operator(context, from, params, ctes, prune, filters)
            }
            SourcePlan::Join { .. } => build_join_source_operator(
                context,
                from,
                params,
                ctes,
                prune,
                filters,
                recheck_path,
                bindings,
            ),
            SourcePlan::Values { .. } => {
                build_values_source_operator(context, from, params, ctes, prune, filters)
            }
            SourcePlan::Function { .. } => {
                build_function_source_operator(context, from, params, ctes, prune, filters)
            }
            SourcePlan::FunctionGroup { .. } => {
                build_function_group_source_operator(context, from, params, ctes, prune, filters)
            }
            SourcePlan::Subquery { .. } => build_subquery_source_operator(
                context, from, params, ctes, prune, filters, bindings,
            ),
        }
    })()?;
    let kind = match from {
        SourcePlan::Table {
            executable_query: Some(_),
            ..
        } => "Named Query Scan",
        SourcePlan::Table { .. } => "Relation Scan",
        SourcePlan::Join { .. } => "Join Source",
        SourcePlan::Subquery { .. } => "Subquery Scan",
        SourcePlan::Values { .. } => "Values Scan",
        SourcePlan::RetainedRows(_) => "Retained Rows Scan",
        SourcePlan::Function { .. } => "Function Scan",
        SourcePlan::FunctionGroup { .. } => "Function Group Scan",
    };
    context.scans.runtime.diagnostics.observe(input, kind)
}

/// Keep the analyzed row descriptor while omitting every removed scan's initialization and row effects. SELECT authorization is performed by the containing block before assembly.
pub(crate) fn empty_source_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    outer: Option<&crate::RowSchema>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    let outer = outer.or_else(|| ctes.row_lock_outer_row().map(|row| &row.schema));
    let schema =
        super::bind_source_plan_schema(context.ctes.routines, source, params, ctes, outer)?;
    if schema.columns_are_open(None) {
        return Err(SQLError::Routine {
            sqlstate: "42P18".into(),
            message: "cannot determine runtime table-function columns from an empty input".into(),
        });
    }
    Ok(Box::new(crate::TableScan::from_physical_rows(
        schema,
        Vec::new(),
    )))
}
