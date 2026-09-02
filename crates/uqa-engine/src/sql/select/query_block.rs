//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-block preparation and column-pruning analysis.

use super::{
    bind_source_plan_schema, execute_query_block_operator_output, expand_from_star_columns,
    expr_contains_subquery, expr_contains_volatile_function, final_filter_after_qualifier_pushdown,
    has_window, overlay_outer_schema, projection_columns, qualifier_filters_for_stmt,
    resolve_row_locks, run_select_without_from_output, run_single_foreign_select_output,
    run_single_table_select_output, validate_query_block_expression_types,
    validate_query_block_references, validate_query_set_contexts,
    validate_source_set_contexts_before_build, with_query_table_pseudo_columns, BTreeSet,
    ColumnPrune, CteScope, Engine, QueryBlockPlan, QueryOutput, QueryOutputMode, SQLError,
    SQLParam, ScalarExpr, ScopedEngineHook, SingleRelation, SourcePlan,
};
use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};
use crate::sql::from_rows::SourceProjection;
use crate::sql::{
    DOC_ID_COLUMN, META_DOC_ID_COLUMN, META_QUALIFIER, META_SCORE_COLUMN, SCORE_COLUMN,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub(in crate::sql) fn run_query_block_with_prepared_exists_output(
    engine: &Engine,
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let outer = ctes.row_lock_outer_row().map(|row| row.schema.clone());
    let source_schema = stmt.from.as_ref().map_or_else(
        || Ok(uqa_execution::RowSchema::default()),
        |source| bind_source_plan_schema(engine, source, params, ctes, outer.as_ref()),
    )?;
    let source_schema = with_query_table_pseudo_columns(&source_schema);
    let expression_schema = overlay_outer_schema(&source_schema, outer.as_ref());
    validate_query_block_expression_types(engine, stmt, &expression_schema, params, ctes)?;
    let type_resolver = ScopedEngineHook::new(engine, ctes);
    validate_query_set_contexts(engine, &type_resolver, stmt, &expression_schema, params)?;

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
        validate_query_block_references(engine, stmt, &expression_schema, params, ctes)?;
        return run_select_without_from_output(engine, block, stmt, params, ctes, output_mode);
    };
    validate_source_set_contexts_before_build(
        engine,
        &type_resolver,
        from,
        params,
        ctes,
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
        include_descendants,
    } = from
    {
        if !ctes.is_visible_cte(name) {
            let catalog = ctes.catalog_read_view()?;
            let resolution = ctes.relation_name_resolution()?;
            let foreign_table = catalog.foreign_table_entry_resolved(&resolution, name)?;
            if alias.is_none() && foreign_table.is_some() {
                let foreign_name = foreign_table
                    .as_ref()
                    .map(|(canonical, _)| canonical.as_str())
                    .expect("foreign table presence checked above");
                validate_query_block_references(engine, stmt, &expression_schema, params, ctes)?;
                return run_single_foreign_select_output(
                    engine,
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
                ctes.reads_command_overlay() && engine.command_mutation_overlay_active();
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
                    crate::sql::validate_expr_text_match_fields(engine, local_table, filter)?;
                }
                let reference_schema = if schemaless {
                    schemaless_reference_schema(
                        engine,
                        stmt,
                        from,
                        qualifier,
                        outer.as_ref(),
                        ctes,
                    )?
                } else {
                    expression_schema.clone()
                };
                validate_query_block_references(engine, stmt, &reference_schema, params, ctes)?;
                return run_single_table_select_output(
                    engine,
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
        crate::sql::validate_joined_expr_text_match_fields(engine, from, filter)?;
    }

    let column_prune = column_prune_for_stmt(engine, stmt, from, ctes)?;
    let qualifier_filters = qualifier_filters_for_stmt(engine, stmt, from, ctes)?;
    let source_row_locks = resolve_row_locks(
        engine,
        from,
        &stmt.locking,
        stmt.r#where.as_ref(),
        params,
        ctes,
    )?;
    let operator = {
        let mut scoped_ctes = ctes.enter_source_row_locks(source_row_locks);
        crate::sql::from_rows::build_join_operator_with_ctes(
            engine,
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
    validate_query_block_references(engine, stmt, &projection_schema, params, ctes)?;
    let physical_filter = final_filter_after_qualifier_pushdown(
        engine,
        stmt,
        from,
        qualifier_filters.as_ref(),
        ctes,
    )?;

    let columns = expand_from_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        &source_schema,
    )?;
    execute_query_block_operator_output(
        engine,
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

/// Bind the query-visible fields of a schemaless table from the same projection contract that constructs its document scan.
fn schemaless_reference_schema(
    engine: &Engine,
    statement: &QueryBlockPlan,
    source: &SourcePlan,
    qualifier: &str,
    outer: Option<&uqa_execution::RowSchema>,
    ctes: &CteScope,
) -> Result<uqa_execution::RowSchema, SQLError> {
    let columns = column_prune_for_stmt(engine, statement, source, ctes)?
        .and_then(|prune| prune.get(qualifier).cloned())
        .and_then(SourceProjection::explicit_columns)
        .map_or_else(Vec::new, |columns| columns.into_iter().collect());
    let schema = uqa_execution::RowSchema::with_qualified_types(
        qualifier,
        columns.clone(),
        vec![None; columns.len()],
    );
    let schema = with_query_table_pseudo_columns(&schema);
    Ok(overlay_outer_schema(&schema, outer))
}

pub(in crate::sql) fn column_prune_for_stmt(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    ctes: &CteScope,
) -> Result<Option<ColumnPrune>, SQLError> {
    column_prune_for_stmt_with_filter(engine, stmt, from, stmt.r#where.as_ref(), ctes)
}

/// Compute the document projection for `stmt` while treating `filter` as the
/// only predicate that remains to be evaluated by the relational pipeline.
/// Accelerated retrieval consumes its search predicate before constructing a
/// [`ScoredDocumentSource`](super::ScoredDocumentSource), so its field
/// arguments are index dependencies rather than row-materialization
/// dependencies. Callers that have executed retrieval pass only the residual
/// predicate here; ordinary scans retain the statement's original `WHERE` via
/// [`column_prune_for_stmt`].
pub(in crate::sql) fn column_prune_for_stmt_with_filter(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filter: Option<&ScalarExpr>,
    ctes: &CteScope,
) -> Result<Option<ColumnPrune>, SQLError> {
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution()?;
    let requires_full_projection = source_contains_join_alias(from)
        || has_window(&stmt.projections)
        || stmt.projections.iter().any(|projection| {
            matches!(projection.expr, ScalarExpr::Star)
                || expr_contains_subquery(&projection.expr)
                || expr_contains_volatile_function(engine, &projection.expr)
        });

    let mut qualifiers = Vec::new();
    collect_from_qualifiers(from, &mut qualifiers);
    if qualifiers.is_empty() {
        return Ok(None);
    }

    let metadata_binding = single_local_table_metadata_binding(&catalog, &resolution, from, ctes)?;
    let scope = PruneScope {
        qualifiers: &qualifiers,
        metadata_qualifier: metadata_binding
            .as_ref()
            .map(|binding| binding.qualifier.as_str()),
        legacy_doc_id: metadata_binding
            .as_ref()
            .is_some_and(|binding| binding.legacy_doc_id),
        legacy_score: metadata_binding
            .as_ref()
            .is_some_and(|binding| binding.legacy_score),
    };
    let mut prune: ColumnPrune = qualifiers
        .iter()
        .map(|qualifier| {
            (
                qualifier.clone(),
                if requires_full_projection {
                    SourceProjection::retaining_all()
                } else {
                    SourceProjection::default()
                },
            )
        })
        .collect();
    let mut valid = true;
    collect_from_prune_columns(from, scope, &mut prune, &mut valid);
    collect_join_binding_prune_columns(&catalog, &resolution, from, &mut prune)?;
    collect_query_block_prune_columns(stmt, filter, scope, &mut prune, &mut valid);
    let metadata_requested = prune
        .values()
        .any(|projection| !projection.metadata().is_empty());
    if requires_full_projection {
        return Ok(metadata_requested.then_some(prune));
    }
    if !valid {
        if metadata_requested {
            for projection in prune.values_mut() {
                projection.retain_all();
            }
            return Ok(Some(prune));
        }
        return Ok(None);
    }
    Ok(Some(prune))
}

#[derive(Clone, Copy)]
struct PruneScope<'a> {
    qualifiers: &'a [String],
    metadata_qualifier: Option<&'a str>,
    legacy_doc_id: bool,
    legacy_score: bool,
}

fn collect_query_block_prune_columns(
    stmt: &QueryBlockPlan,
    filter: Option<&ScalarExpr>,
    scope: PruneScope<'_>,
    prune: &mut ColumnPrune,
    valid: &mut bool,
) {
    let expressions = stmt
        .projections
        .iter()
        .map(|projection| &projection.expr)
        .chain(filter)
        .chain(stmt.group_by.iter())
        .chain(stmt.grouping_sets.iter().flatten())
        .chain(stmt.having.iter())
        .chain(stmt.order_by.iter().map(|order| &order.expr))
        .chain(stmt.distinct_on.iter());
    for expression in expressions {
        collect_expr_prune_columns(expression, scope, prune, valid);
    }
}

struct LocalTableMetadataBinding {
    qualifier: String,
    legacy_doc_id: bool,
    legacy_score: bool,
}

fn single_local_table_metadata_binding(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    source: &SourcePlan,
    ctes: &CteScope,
) -> Result<Option<LocalTableMetadataBinding>, SQLError> {
    if source_contains_join_alias(source) {
        return Ok(None);
    }
    fn collect(
        catalog: &CatalogReadView,
        resolution: &RelationNameResolution,
        source: &SourcePlan,
        ctes: &CteScope,
        relations: &mut BTreeSet<(String, String)>,
    ) -> Result<(), SQLError> {
        match source {
            SourcePlan::Table {
                name,
                qualifier,
                alias,
                ..
            } => {
                if !ctes.is_visible_cte(name) {
                    if let Some(name) = catalog.table_name_resolved(resolution, name)? {
                        relations.insert((alias.as_deref().unwrap_or(qualifier).to_string(), name));
                    }
                }
            }
            SourcePlan::Join { left, right, .. } => {
                collect(catalog, resolution, left, ctes, relations)?;
                collect(catalog, resolution, right, ctes, relations)?;
            }
            SourcePlan::Values { .. }
            | SourcePlan::Function { .. }
            | SourcePlan::FunctionGroup { .. }
            | SourcePlan::Subquery { .. } => {}
        }
        Ok(())
    }
    let mut relations = BTreeSet::new();
    collect(catalog, resolution, source, ctes, &mut relations)?;
    let Some((qualifier, name)) = relations.pop_first() else {
        return Ok(None);
    };
    if !relations.is_empty() {
        return Ok(None);
    }
    let columns = &catalog
        .table_resolved(resolution, &name)?
        .ok_or_else(|| SQLError::UnknownTable(name.clone()))?
        .columns;
    Ok(Some(LocalTableMetadataBinding {
        qualifier,
        legacy_doc_id: !columns.iter().any(|column| column.name == DOC_ID_COLUMN),
        legacy_score: !columns.iter().any(|column| column.name == SCORE_COLUMN),
    }))
}

fn source_contains_join_alias(source: &SourcePlan) -> bool {
    match source {
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            alias.is_some() || source_contains_join_alias(left) || source_contains_join_alias(right)
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => false,
    }
}

fn collect_join_binding_prune_columns(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    from: &SourcePlan,
    prune: &mut ColumnPrune,
) -> Result<(), SQLError> {
    match from {
        SourcePlan::Join {
            left,
            right,
            using,
            natural,
            ..
        } => {
            collect_join_binding_prune_columns(catalog, resolution, left, prune)?;
            collect_join_binding_prune_columns(catalog, resolution, right, prune)?;
            if let Some(using) = using {
                for column in &using.columns {
                    add_column_to_source_prune(left, column, prune);
                    add_column_to_source_prune(right, column, prune);
                }
            }
            if *natural {
                add_all_source_columns_to_prune(catalog, resolution, left, prune)?;
                add_all_source_columns_to_prune(catalog, resolution, right, prune)?;
            }
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => {}
    }
    Ok(())
}

fn add_column_to_source_prune(source: &SourcePlan, column: &str, prune: &mut ColumnPrune) {
    let mut qualifiers = Vec::new();
    collect_from_qualifiers(source, &mut qualifiers);
    for qualifier in qualifiers {
        if let Some(columns) = prune.get_mut(&qualifier) {
            columns.insert(column.to_string());
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
fn add_all_source_columns_to_prune(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    source: &SourcePlan,
    prune: &mut ColumnPrune,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            ..
        } => {
            let qualifier = alias.as_deref().unwrap_or(qualifier);
            match catalog.table_resolved(resolution, name)? {
                Some(table) => {
                    if let Some(columns) = prune.get_mut(qualifier) {
                        columns.extend(table.columns.iter().map(|column| column.name.clone()));
                    }
                }
                None => {
                    // A CTE, view, or external relation owns its row type
                    // outside the local table catalog. Omitting its prune
                    // entry retains that source's complete schema.
                    prune.remove(qualifier);
                }
            }
        }
        SourcePlan::Join { left, right, .. } => {
            add_all_source_columns_to_prune(catalog, resolution, left, prune)?;
            add_all_source_columns_to_prune(catalog, resolution, right, prune)?;
        }
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
            ..
        } => {
            let Some(columns) = alias.as_ref().and_then(|alias| prune.get_mut(alias)) else {
                return Ok(());
            };
            if column_aliases.is_empty() {
                columns.extend(
                    (0..rows.first().map_or(0, Vec::len))
                        .map(|index| format!("column{}", index + 1)),
                );
            } else {
                columns.extend(column_aliases.iter().cloned());
            }
        }
        SourcePlan::Function {
            name,
            output_name,
            args,
            alias,
            column_aliases,
            ordinality,
            ..
        } => {
            let qualifier = alias.as_ref().unwrap_or(output_name);
            let Some(columns) = prune.get_mut(qualifier) else {
                return Ok(());
            };
            let routine_columns = super::user_function_output_columns(catalog, resolution, name)?;
            columns.extend(routine_columns.map_or_else(
                || {
                    crate::sql::from_rows::table_function_empty_schema(
                        name,
                        output_name,
                        alias.as_deref(),
                        column_aliases,
                        args.len(),
                        *ordinality,
                    )
                },
                |base| {
                    crate::sql::from_rows::apply_table_function_aliases(
                        base,
                        column_aliases,
                        *ordinality,
                    )
                },
            ));
        }
        SourcePlan::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } => {
            let Some(qualifier) = alias
                .as_ref()
                .or_else(|| functions.first().map(|function| &function.output_name))
            else {
                return Ok(());
            };
            let Some(columns) = prune.get_mut(qualifier) else {
                return Ok(());
            };
            let mut group_columns = Vec::new();
            for function in functions {
                let routine_columns =
                    super::user_function_output_columns(catalog, resolution, &function.name)?;
                group_columns.extend(routine_columns.map_or_else(
                    || {
                        crate::sql::from_rows::table_function_empty_schema(
                            &function.name,
                            &function.output_name,
                            None,
                            &function.column_aliases,
                            function.args.len(),
                            false,
                        )
                    },
                    |base| {
                        crate::sql::from_rows::apply_table_function_aliases(
                            base,
                            &function.column_aliases,
                            false,
                        )
                    },
                ));
            }
            if *ordinality {
                group_columns.push("ordinality".into());
            }
            for (column, alias) in group_columns.iter_mut().zip(column_aliases) {
                column.clone_from(alias);
            }
            columns.extend(group_columns);
        }
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            let Some(columns) = alias.as_ref().and_then(|alias| prune.get_mut(alias)) else {
                return Ok(());
            };
            if column_aliases.is_empty() {
                columns.extend(super::query_plan_output_columns(body).unwrap_or_default());
            } else {
                columns.extend(column_aliases.iter().cloned());
            }
        }
    }
    Ok(())
}

pub(in crate::sql) fn collect_from_qualifiers(from: &SourcePlan, out: &mut Vec<String>) {
    match from {
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if let Some(alias) = alias {
                out.push(alias.clone());
            } else {
                collect_from_qualifiers(left, out);
                collect_from_qualifiers(right, out);
            }
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => {
            if let Some(qualifier) = from.visible_qualifier() {
                out.push(qualifier.to_string());
            }
        }
    }
}

fn collect_from_prune_columns(
    from: &SourcePlan,
    scope: PruneScope<'_>,
    prune: &mut ColumnPrune,
    valid: &mut bool,
) {
    match from {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            collect_from_prune_columns(left, scope, prune, valid);
            collect_from_prune_columns(right, scope, prune, valid);
            if let Some(on) = on.as_ref() {
                collect_expr_prune_columns(on, scope, prune, valid);
            }
        }
        SourcePlan::Values { rows, .. } => {
            for row in rows {
                for expr in row {
                    collect_expr_prune_columns(expr, scope, prune, valid);
                }
            }
        }
        SourcePlan::Function { args, .. } => {
            for expr in args {
                collect_expr_prune_columns(expr, scope, prune, valid);
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                for expr in &function.args {
                    collect_expr_prune_columns(expr, scope, prune, valid);
                }
            }
        }
        SourcePlan::Subquery { .. } => {
            *valid = false;
        }
        SourcePlan::Table { .. } => {}
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
fn collect_expr_prune_columns(
    expr: &ScalarExpr,
    scope: PruneScope<'_>,
    prune: &mut ColumnPrune,
    valid: &mut bool,
) {
    match expr {
        ScalarExpr::Column(column) => {
            if let Some(qualifier) = scope.metadata_qualifier {
                let metadata = match column.as_str() {
                    DOC_ID_COLUMN if scope.legacy_doc_id => Some(true),
                    SCORE_COLUMN if scope.legacy_score => Some(false),
                    _ => None,
                };
                if let Some(doc_id) = metadata {
                    let Some(source) = prune.get_mut(qualifier) else {
                        *valid = false;
                        return;
                    };
                    source.insert(column.clone());
                    if doc_id {
                        source.metadata_mut().request_doc_id();
                    } else {
                        source.metadata_mut().request_score();
                    }
                    return;
                }
            }
            for qualifier in scope.qualifiers {
                if let Some(columns) = prune.get_mut(qualifier) {
                    columns.insert(column.clone());
                }
            }
        }
        ScalarExpr::QualifiedColumn {
            qualifier, column, ..
        } => {
            if scope.metadata_qualifier == Some(qualifier.as_str()) {
                let metadata = match column.as_str() {
                    DOC_ID_COLUMN if scope.legacy_doc_id => Some(true),
                    SCORE_COLUMN if scope.legacy_score => Some(false),
                    _ => None,
                };
                if let Some(doc_id) = metadata {
                    let Some(source) = prune.get_mut(qualifier) else {
                        *valid = false;
                        return;
                    };
                    source.insert(column.clone());
                    if doc_id {
                        source.metadata_mut().request_doc_id();
                    } else {
                        source.metadata_mut().request_score();
                    }
                    return;
                }
            }
            if qualifier == META_QUALIFIER && !prune.contains_key(META_QUALIFIER) {
                let Some(source) = scope
                    .metadata_qualifier
                    .and_then(|source| prune.get_mut(source))
                else {
                    *valid = false;
                    return;
                };
                match column.as_str() {
                    META_DOC_ID_COLUMN => source.metadata_mut().request_doc_id(),
                    META_SCORE_COLUMN => source.metadata_mut().request_score(),
                    _ => *valid = false,
                }
                return;
            }
            if let Some(columns) = prune.get_mut(qualifier) {
                columns.insert(column.clone());
            } else {
                *valid = false;
            }
        }
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => {}
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {
            *valid = false;
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            for item in items {
                collect_expr_prune_columns(item, scope, prune, valid);
            }
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for arg in args {
                collect_expr_prune_columns(arg, scope, prune, valid);
            }
            for order in order_by {
                collect_expr_prune_columns(&order.expr, scope, prune, valid);
            }
            if let Some(filter) = filter.as_ref() {
                collect_expr_prune_columns(filter, scope, prune, valid);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_expr_prune_columns(lhs, scope, prune, valid);
            collect_expr_prune_columns(rhs, scope, prune, valid);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            collect_expr_prune_columns(inner, scope, prune, valid);
        }
        ScalarExpr::Between { expr, low, high } => {
            collect_expr_prune_columns(expr, scope, prune, valid);
            collect_expr_prune_columns(low, scope, prune, valid);
            collect_expr_prune_columns(high, scope, prune, valid);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_expr_prune_columns(expr, scope, prune, valid);
            for item in list {
                collect_expr_prune_columns(item, scope, prune, valid);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_expr_prune_columns(argument, scope, prune, valid);
            }
            for expression in &spec.partition_by {
                collect_expr_prune_columns(expression, scope, prune, valid);
            }
            for order in &spec.order_by {
                collect_expr_prune_columns(&order.expr, scope, prune, valid);
            }
            if let Some(frame) = &spec.frame {
                for bound in [&frame.start, &frame.end] {
                    if let uqa_execution::ScalarFrameBound::Preceding(expression)
                    | uqa_execution::ScalarFrameBound::Following(expression) = bound
                    {
                        collect_expr_prune_columns(expression, scope, prune, valid);
                    }
                }
            }
            *valid = false;
        }
        ScalarExpr::InSubquery { expr, .. } => {
            collect_expr_prune_columns(expr, scope, prune, valid);
            *valid = false;
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_ref() {
                collect_expr_prune_columns(base, scope, prune, valid);
            }
            for (cond, result) in when {
                collect_expr_prune_columns(cond, scope, prune, valid);
                collect_expr_prune_columns(result, scope, prune, valid);
            }
            if let Some(else_branch) = else_branch.as_ref() {
                collect_expr_prune_columns(else_branch, scope, prune, valid);
            }
        }
    }
}
