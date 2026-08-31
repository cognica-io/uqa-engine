//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source row-type adapters and join binding inputs.

use uqa_execution::RowSchema;
use uqa_planner::{QueryPlan, SourcePlan, TableFunctionPlan};
use uqa_sql::ast::{JoinKind, JoinUsing};
use uqa_sql::{SQLError, SQLParam};

use super::{analysis, CteScope, ScalarExpr, SchemaScope};
use crate::engine_user_functions::RoutineResolution;

pub(super) struct JoinSchemaBinding<'a> {
    pub(super) routines: &'a dyn RoutineResolution,
    pub(super) kind: JoinKind,
    pub(super) on: Option<&'a ScalarExpr>,
    pub(super) using: Option<&'a JoinUsing>,
    pub(super) natural: bool,
    pub(super) alias: Option<&'a str>,
    pub(super) column_aliases: &'a [String],
    pub(super) left: &'a RowSchema,
    pub(super) right: &'a RowSchema,
    pub(super) subqueries: &'a [QueryPlan],
    pub(super) params: &'a [SQLParam],
    pub(super) outer: Option<&'a RowSchema>,
}

pub(super) fn table_function_member_source(function: &TableFunctionPlan) -> SourcePlan {
    SourcePlan::Function {
        name: function.name.clone(),
        binding: function.binding.clone(),
        output_name: function.output_name.clone(),
        relation: function.relation.clone(),
        args: function.args.clone(),
        alias: None,
        column_aliases: function.column_aliases.clone(),
        ordinality: false,
        column_types: function.column_types.clone(),
    }
}

/// Derive the exact row type of one FROM source without executing it.
pub(in crate::sql) fn bind_source_plan_schema(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_execution_scope(ctes)?.bind_source(
        routines,
        source,
        &ctes.scalar_subqueries,
        params,
        outer,
    )
}

/// Add query-block pseudo columns after the complete source scope is known, so `_meta` is exposed only for one unambiguous local-table source and never shadows a real relation alias.
pub(in crate::sql) fn with_query_table_pseudo_columns(schema: &RowSchema) -> RowSchema {
    analysis::with_unqualified_table_pseudo_columns(schema)
}

/// Derive and validate one FROM source's exact row type without executing it.
pub(in crate::sql) fn analyze_source_plan_schema(
    routines: &dyn RoutineResolution,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_analysis(ctes)?.bind_source(
        routines,
        source,
        &ctes.scalar_subqueries,
        params,
        outer,
    )
}

/// Bind every table-function source in one execution-owned source plan to its exact routine identity and return the schema derived from those same bindings.
pub(in crate::sql) fn bind_source_plan_schema_for_execution(
    routines: &dyn RoutineResolution,
    source: &mut SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_execution_scope(ctes)?.bind_source_for_execution(
        routines,
        source,
        &ctes.scalar_subqueries,
        params,
        outer,
    )
}
