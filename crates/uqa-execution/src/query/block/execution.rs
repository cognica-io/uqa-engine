//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-block validation and physical source selection.

use super::{
    bind_source_plan_schema, bind_source_plan_schema_for_execution, binding_context,
    ensure_select_privileges_for_query_block, execute_query_block_operator_output,
    expand_from_star_columns, overlay_outer_schema, projection_columns,
    run_select_without_from_output, run_single_foreign_select_output,
    run_single_table_select_output, validate_query_block_expression_types,
    validate_query_block_references, validate_query_set_contexts,
    validate_source_set_contexts_before_build, with_query_table_pseudo_columns, ComputePlan,
    CteScope, QueryBlockPlan, QueryOutput, QueryOutputMode, SQLError, SQLParam, ScalarExpr,
    SingleRelation, SourceContext, SourcePlan, SourceProjection,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub fn run_query_block_with_prepared_exists_output<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    block: &'a QueryBlockPlan,
    stmt: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a mut CteScope<S>,
    output_mode: QueryOutputMode<'a>,
) -> Result<QueryOutput, SQLError> {
    let outer = ctes.row_lock_outer_row().map(|row| row.schema.clone());
    let source_schema = stmt.from.as_ref().map_or_else(
        || Ok(crate::RowSchema::default()),
        |source| {
            bind_source_plan_schema(context.ctes.routines, source, params, ctes, outer.as_ref())
        },
    )?;
    let source_schema = with_query_table_pseudo_columns(&source_schema);
    let expression_schema = overlay_outer_schema(&source_schema, outer.as_ref());
    validate_query_block_expression_types(
        context.ctes.routines,
        stmt,
        &expression_schema,
        params,
        ctes,
    )?;
    let type_resolver = context.relational.expression_scope(ctes.clone());
    validate_query_set_contexts(
        context.relational.catalog,
        type_resolver.as_ref(),
        stmt,
        &expression_schema,
        params,
    )?;

    let Some(from) = stmt.from.as_ref() else {
        if stmt
            .projections
            .iter()
            .any(|projection| matches!(projection.expr, ScalarExpr::Star))
            && outer.is_none()
        {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "SELECT * with no tables specified is not valid".into(),
            });
        }
        validate_query_block_references(
            context.ctes.routines,
            stmt,
            &expression_schema,
            params,
            ctes,
        )?;
        return run_select_without_from_output(context, block, stmt, params, ctes, output_mode);
    };
    validate_source_set_contexts_before_build(
        context.relational.catalog,
        type_resolver.as_ref(),
        from,
        params,
        &binding_context(ctes)?,
        outer.as_ref(),
    )?;

    // Set-op branches, CTEs, and derived-table bodies still need the same
    // search-aware single-table physical access path as top-level queries;
    // otherwise registry-backed predicates such as
    // `pool_positive_evidence(bayesian_match(...), knn_match(...))` fall
    // through to scalar expression evaluation.
    if let SourcePlan::Table {
        name,
        qualifier,
        alias,
        column_aliases,
        include_descendants,
        ..
    } = from
    {
        if !ctes.is_visible_cte(name) {
            let catalog = ctes.catalog_read_view()?;
            let resolution = ctes.relation_name_resolution()?;
            let foreign_table = catalog.foreign_table_entry_resolved(&resolution, name)?;
            if alias.is_none() && column_aliases.is_empty() && foreign_table.is_some() {
                let foreign_name = foreign_table
                    .as_ref()
                    .map(|(canonical, _)| canonical.as_str())
                    .expect("foreign table presence checked above");
                validate_query_block_references(
                    context.ctes.routines,
                    stmt,
                    &expression_schema,
                    params,
                    ctes,
                )?;
                ensure_select_privileges_for_query_block(stmt, from, ctes)?;
                return run_single_foreign_select_output(
                    context,
                    SingleRelation {
                        reference_name: name,
                        relation_name: foreign_name,
                        qualifier,
                    },
                    block,
                    stmt,
                    params,
                    ctes,
                    output_mode,
                );
            }
            let local_table = catalog.table_name_resolved(&resolution, name)?;
            let is_virtual =
                name.contains('.') || (local_table.is_none() && foreign_table.is_none());
            let schemaless = local_table.is_some()
                && catalog
                    .table_resolved(&resolution, name)?
                    .is_some_and(|table| table.columns.is_empty());
            let command_overlay =
                ctes.reads_command_overlay() && context.documents.command_overlay_active();
            let has_hierarchy_descendants = local_table.is_some()
                && catalog
                    .hierarchy_scan_tables(&resolution, name, *include_descendants)?
                    .len()
                    > 1;
            if alias.is_none() && !is_virtual && !command_overlay && !has_hierarchy_descendants {
                let local_table = local_table
                    .as_deref()
                    .expect("single-table fast path requires a resolved table");
                if let Some(filter) = stmt.r#where.as_ref() {
                    uqa_sql::semantics::text_indexes::validate_expr_text_match_fields(
                        context.text_indexes,
                        local_table,
                        filter,
                    )?;
                }
                let reference_schema = if schemaless {
                    schemaless_reference_schema(
                        context,
                        stmt,
                        from,
                        qualifier,
                        outer.as_ref(),
                        ctes,
                    )?
                } else {
                    expression_schema.clone()
                };
                validate_query_block_references(
                    context.ctes.routines,
                    stmt,
                    &reference_schema,
                    params,
                    ctes,
                )?;
                ensure_select_privileges_for_query_block(stmt, from, ctes)?;
                return run_single_table_select_output(
                    context,
                    SingleRelation {
                        reference_name: name,
                        relation_name: local_table,
                        qualifier,
                    },
                    block,
                    stmt,
                    params,
                    ctes,
                    output_mode,
                );
            }
        }
    }

    if let Some(filter) = stmt.r#where.as_ref() {
        uqa_sql::semantics::text_indexes::validate_joined_expr_text_match_fields(
            context.text_indexes,
            from,
            filter,
        )?;
    }

    let reference_schema_is_execution_defined = stmt
        .r#where
        .as_ref()
        .is_some_and(uqa_sql::semantics::contains_retrieval)
        || source_schema_is_execution_defined(context.relational.runtime, from);
    if !reference_schema_is_execution_defined {
        validate_query_block_references(
            context.ctes.routines,
            stmt,
            &expression_schema,
            params,
            ctes,
        )?;
    }
    let column_prune = context.planning.column_prune(stmt, from, ctes)?;
    let qualifier_filters = context.planning.qualifier_filters(stmt, from, ctes)?;
    let source_row_locks = crate::query::locking::resolve_row_locks(
        context.locking,
        from,
        &stmt.locking,
        stmt.r#where.as_ref(),
        params,
        ctes,
    )?;
    ensure_select_privileges_for_query_block(stmt, from, ctes)?;
    let operator = {
        let mut scoped_ctes = ctes.enter_source_row_locks(source_row_locks);
        crate::query::sources::build_join_operator_with_ctes(
            context,
            from,
            params,
            &mut scoped_ctes,
            column_prune.as_ref(),
            qualifier_filters.as_ref(),
        )?
    };
    let source_schema = operator.row_schema().clone();
    let projection_schema = with_query_table_pseudo_columns(&source_schema);
    let projection_schema = overlay_outer_schema(&projection_schema, outer.as_ref());
    validate_query_block_references(
        context.ctes.routines,
        stmt,
        &projection_schema,
        params,
        ctes,
    )?;
    let physical_filter =
        context
            .planning
            .residual_filter(stmt, from, qualifier_filters.as_ref(), ctes)?;

    let columns = expand_from_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        &source_schema,
    )?;
    execute_query_block_operator_output(
        context.relational,
        operator,
        physical_filter,
        stmt,
        block,
        params,
        ctes,
        columns,
        output_mode,
    )
}

/// Whether a source contains an extension table function whose row shape is declared by the function result rather than by SQL metadata.
fn source_schema_is_execution_defined(
    context: crate::query::runtime::QueryRuntimeView<'_>,
    source: &SourcePlan,
) -> bool {
    match source {
        SourcePlan::Join { left, right, .. } => {
            source_schema_is_execution_defined(context, left)
                || source_schema_is_execution_defined(context, right)
        }
        SourcePlan::Function { name, binding, .. } => {
            binding.as_ref().is_none_or(|binding| binding.builtin)
                && context.has_table_function(&name.to_ascii_lowercase())
        }
        SourcePlan::FunctionGroup { functions, .. } => functions.iter().any(|function| {
            function
                .binding
                .as_ref()
                .is_none_or(|binding| binding.builtin)
                && context.has_table_function(&function.name.to_ascii_lowercase())
        }),
        SourcePlan::Subquery { body, .. } => query_schema_is_execution_defined(context, body),
        SourcePlan::Table { .. } | SourcePlan::Values { .. } => false,
    }
}

fn query_schema_is_execution_defined(
    context: crate::query::runtime::QueryRuntimeView<'_>,
    query: &uqa_sql::plan::QueryPlan,
) -> bool {
    match &query.root {
        uqa_sql::plan::RelationalPlan::QueryBlock(block) => block
            .from
            .as_ref()
            .is_some_and(|source| source_schema_is_execution_defined(context, source)),
        uqa_sql::plan::RelationalPlan::SetOp { left, right, .. } => {
            query_schema_is_execution_defined(context, left)
                || query_schema_is_execution_defined(context, right)
        }
        uqa_sql::plan::RelationalPlan::Values { .. } => false,
    }
}

/// Bind the query-visible fields of a schemaless table from the same projection contract that constructs its document scan.
fn schemaless_reference_schema<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    statement: &QueryBlockPlan,
    source: &SourcePlan,
    qualifier: &str,
    outer: Option<&crate::RowSchema>,
    ctes: &CteScope<S>,
) -> Result<crate::RowSchema, SQLError> {
    let columns = context
        .planning
        .column_prune(statement, source, ctes)?
        .and_then(|prune| prune.get(qualifier).cloned())
        .and_then(SourceProjection::explicit_columns)
        .map_or_else(Vec::new, |columns| columns.into_iter().collect());
    let schema = crate::RowSchema::with_qualified_types(
        qualifier,
        columns.clone(),
        vec![None; columns.len()],
    );
    let schema = with_query_table_pseudo_columns(&schema);
    Ok(overlay_outer_schema(&schema, outer))
}

pub fn execute_query_block_output<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    block: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    output_mode: QueryOutputMode<'a>,
) -> Result<QueryOutput, SQLError> {
    let inherited_lock_identities = ctes.lock_identities.emit;
    let mut scoped_ctes = ctes.enter_scalar_subqueries(&block.subqueries);
    let row_identity_barrier = block.distinct
        || !block.distinct_on.is_empty()
        || matches!(block.compute, ComputePlan::Aggregate | ComputePlan::Window);
    scoped_ctes.lock_identities.emit =
        !block.locking.is_empty() || (inherited_lock_identities && !row_identity_barrier);
    scoped_ctes.lock_identities.retain_after_lock =
        inherited_lock_identities && !row_identity_barrier;
    let defer_distinct_limit = uqa_sql::semantics::should_defer_distinct_limit(block);
    let mut execution = uqa_sql::semantics::select_execution_stmt(block, defer_distinct_limit);
    let outer = scoped_ctes.row_lock_outer_row().map(|row| &row.schema);
    if let Some(source) = execution.from.as_mut() {
        bind_source_plan_schema_for_execution(
            context.ctes.routines,
            source,
            params,
            &scoped_ctes,
            outer,
        )?;
    }
    run_query_block_with_prepared_exists_output(
        context,
        block,
        &execution,
        params,
        &mut scoped_ctes,
        output_mode,
    )
}
