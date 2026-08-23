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
    validate_query_set_contexts, validate_source_set_contexts_before_build, BTreeSet, ColumnPrune,
    CteScope, Engine, QueryBlockPlan, QueryOutput, QueryOutputMode, SQLError, SQLParam, ScalarExpr,
    SingleRelation, SourcePlan,
};

pub(in crate::sql) fn run_query_block_with_prepared_exists_output(
    engine: &Engine,
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let outer = ctes.row_lock_outer_row().map(|row| &row.schema);
    let source_schema = stmt.from.as_ref().map_or_else(
        || Ok(uqa_execution::RowSchema::default()),
        |source| bind_source_plan_schema(engine, source, params, ctes, outer),
    )?;
    let expression_schema = overlay_outer_schema(&source_schema, outer);
    validate_query_block_expression_types(engine, stmt, &expression_schema, params, ctes)?;
    validate_query_set_contexts(engine, stmt, &expression_schema, params)?;

    let Some(from) = stmt.from.as_ref() else {
        return run_select_without_from_output(engine, block, stmt, params, ctes, output_mode);
    };
    validate_source_set_contexts_before_build(engine, from, params, ctes, outer)?;

    // Set-op branches, CTEs, and derived-table bodies still need the same
    // search-aware single-table physical access path as top-level queries;
    // otherwise registry-backed predicates such as
    // `pool_positive_evidence(bayesian_match(...), knn_match(...))` fall
    // through to scalar expression evaluation.
    if let SourcePlan::Table {
        name,
        qualifier,
        alias,
    } = from
    {
        let foreign_table = engine
            .foreign_table(name)
            .map_err(|err| SQLError::Internal(format!("resolve foreign table `{name}`: {err}")))?;
        if alias.is_none() && foreign_table.is_some() {
            return run_single_foreign_select_output(
                engine,
                SingleRelation {
                    storage_name: name,
                    qualifier,
                },
                block,
                stmt,
                params,
                ctes,
                output_mode,
            );
        }
        let local_table = engine
            .try_table(name)
            .map_err(|err| SQLError::Internal(format!("resolve table `{name}`: {err}")))?;
        let is_virtual = name.contains('.') || (local_table.is_none() && foreign_table.is_none());
        let command_overlay =
            ctes.reads_command_overlay() && engine.command_mutation_overlay_active();
        if alias.is_none() && !is_virtual && !command_overlay {
            return run_single_table_select_output(
                engine,
                SingleRelation {
                    storage_name: name,
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

    if let Some(filter) = stmt.r#where.as_ref() {
        crate::sql::validate_joined_expr_text_match_fields(engine, from, filter)?;
    }

    let column_prune = column_prune_for_stmt(engine, stmt, from);
    let qualifier_filters = qualifier_filters_for_stmt(engine, stmt, from);
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
    let physical_filter =
        final_filter_after_qualifier_pushdown(engine, stmt, from, qualifier_filters.as_ref());

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

pub(in crate::sql) fn column_prune_for_stmt(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
) -> Option<ColumnPrune> {
    column_prune_for_stmt_with_filter(engine, stmt, from, stmt.r#where.as_ref())
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
) -> Option<ColumnPrune> {
    if source_contains_join_alias(from)
        || has_window(&stmt.projections)
        || stmt.projections.iter().any(|projection| {
            matches!(projection.expr, ScalarExpr::Star)
                || expr_contains_subquery(&projection.expr)
                || expr_contains_volatile_function(engine, &projection.expr)
        })
    {
        return None;
    }

    let mut qualifiers = Vec::new();
    collect_from_qualifiers(from, &mut qualifiers);
    if qualifiers.is_empty() {
        return None;
    }

    let mut prune: ColumnPrune = qualifiers
        .iter()
        .map(|qualifier| (qualifier.clone(), BTreeSet::new()))
        .collect();
    let mut valid = true;
    collect_from_prune_columns(from, &qualifiers, &mut prune, &mut valid);
    collect_join_binding_prune_columns(engine, from, &mut prune);
    for projection in &stmt.projections {
        collect_expr_prune_columns(&projection.expr, &qualifiers, &mut prune, &mut valid);
    }
    if let Some(filter) = filter {
        collect_expr_prune_columns(filter, &qualifiers, &mut prune, &mut valid);
    }
    for expr in &stmt.group_by {
        collect_expr_prune_columns(expr, &qualifiers, &mut prune, &mut valid);
    }
    for set in &stmt.grouping_sets {
        for expr in set {
            collect_expr_prune_columns(expr, &qualifiers, &mut prune, &mut valid);
        }
    }
    if let Some(having) = stmt.having.as_ref() {
        collect_expr_prune_columns(having, &qualifiers, &mut prune, &mut valid);
    }
    for order in &stmt.order_by {
        collect_expr_prune_columns(&order.expr, &qualifiers, &mut prune, &mut valid);
    }
    for expr in &stmt.distinct_on {
        collect_expr_prune_columns(expr, &qualifiers, &mut prune, &mut valid);
    }
    if !valid {
        return None;
    }
    Some(prune)
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
        | SourcePlan::Subquery { .. } => false,
    }
}

fn collect_join_binding_prune_columns(engine: &Engine, from: &SourcePlan, prune: &mut ColumnPrune) {
    match from {
        SourcePlan::Join {
            left,
            right,
            using,
            natural,
            ..
        } => {
            collect_join_binding_prune_columns(engine, left, prune);
            collect_join_binding_prune_columns(engine, right, prune);
            if let Some(using) = using {
                for column in &using.columns {
                    add_column_to_source_prune(left, column, prune);
                    add_column_to_source_prune(right, column, prune);
                }
            }
            if *natural {
                add_all_source_columns_to_prune(engine, left, prune);
                add_all_source_columns_to_prune(engine, right, prune);
            }
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::Subquery { .. } => {}
    }
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

fn add_all_source_columns_to_prune(engine: &Engine, source: &SourcePlan, prune: &mut ColumnPrune) {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
        } => {
            let qualifier = alias.as_deref().unwrap_or(qualifier);
            match engine.try_table_columns(name) {
                Ok(table_columns) => {
                    if let Some(columns) = prune.get_mut(qualifier) {
                        columns.extend(table_columns);
                    }
                }
                Err(_) => {
                    // A CTE, view, or external relation owns its row type
                    // outside the local table catalog. Omitting its prune
                    // entry retains that source's complete schema.
                    prune.remove(qualifier);
                }
            }
        }
        SourcePlan::Join { left, right, .. } => {
            add_all_source_columns_to_prune(engine, left, prune);
            add_all_source_columns_to_prune(engine, right, prune);
        }
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
        } => {
            let Some(columns) = alias.as_ref().and_then(|alias| prune.get_mut(alias)) else {
                return;
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
                return;
            };
            columns.extend(
                super::user_function_output_columns(engine, name).map_or_else(
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
                ),
            );
        }
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            let Some(columns) = alias.as_ref().and_then(|alias| prune.get_mut(alias)) else {
                return;
            };
            if column_aliases.is_empty() {
                columns.extend(super::query_plan_output_columns(body).unwrap_or_default());
            } else {
                columns.extend(column_aliases.iter().cloned());
            }
        }
    }
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
        | SourcePlan::Subquery { .. } => {
            if let Some(qualifier) = from.visible_qualifier() {
                out.push(qualifier.to_string());
            }
        }
    }
}

pub(in crate::sql) fn collect_from_prune_columns(
    from: &SourcePlan,
    qualifiers: &[String],
    prune: &mut ColumnPrune,
    valid: &mut bool,
) {
    match from {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            collect_from_prune_columns(left, qualifiers, prune, valid);
            collect_from_prune_columns(right, qualifiers, prune, valid);
            if let Some(on) = on.as_ref() {
                collect_expr_prune_columns(on, qualifiers, prune, valid);
            }
        }
        SourcePlan::Values { rows, .. } => {
            for row in rows {
                for expr in row {
                    collect_expr_prune_columns(expr, qualifiers, prune, valid);
                }
            }
        }
        SourcePlan::Function { args, .. } => {
            for expr in args {
                collect_expr_prune_columns(expr, qualifiers, prune, valid);
            }
        }
        SourcePlan::Subquery { .. } => {
            *valid = false;
        }
        SourcePlan::Table { .. } => {}
    }
}

pub(in crate::sql) fn collect_expr_prune_columns(
    expr: &ScalarExpr,
    qualifiers: &[String],
    prune: &mut ColumnPrune,
    valid: &mut bool,
) {
    match expr {
        ScalarExpr::Column(column) => {
            for qualifier in qualifiers {
                if let Some(columns) = prune.get_mut(qualifier) {
                    columns.insert(column.clone());
                }
            }
        }
        ScalarExpr::QualifiedColumn {
            qualifier, column, ..
        } => {
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
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => {
            *valid = false;
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            for item in items {
                collect_expr_prune_columns(item, qualifiers, prune, valid);
            }
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for arg in args {
                collect_expr_prune_columns(arg, qualifiers, prune, valid);
            }
            for order in order_by {
                collect_expr_prune_columns(&order.expr, qualifiers, prune, valid);
            }
            if let Some(filter) = filter.as_ref() {
                collect_expr_prune_columns(filter, qualifiers, prune, valid);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_expr_prune_columns(lhs, qualifiers, prune, valid);
            collect_expr_prune_columns(rhs, qualifiers, prune, valid);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            collect_expr_prune_columns(inner, qualifiers, prune, valid);
        }
        ScalarExpr::Between { expr, low, high } => {
            collect_expr_prune_columns(expr, qualifiers, prune, valid);
            collect_expr_prune_columns(low, qualifiers, prune, valid);
            collect_expr_prune_columns(high, qualifiers, prune, valid);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_expr_prune_columns(expr, qualifiers, prune, valid);
            for item in list {
                collect_expr_prune_columns(item, qualifiers, prune, valid);
            }
        }
        ScalarExpr::WindowCall { .. } => {
            *valid = false;
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_ref() {
                collect_expr_prune_columns(base, qualifiers, prune, valid);
            }
            for (cond, result) in when {
                collect_expr_prune_columns(cond, qualifiers, prune, valid);
                collect_expr_prune_columns(result, qualifiers, prune, valid);
            }
            if let Some(else_branch) = else_branch.as_ref() {
                collect_expr_prune_columns(else_branch, qualifiers, prune, valid);
            }
        }
    }
}
