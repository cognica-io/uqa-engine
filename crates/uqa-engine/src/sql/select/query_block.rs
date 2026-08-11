//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-block preparation and column-pruning analysis.

use super::{
    execute_query_block_operator_output, expand_from_star_columns, expr_contains_subquery,
    expr_contains_volatile_function, final_filter_after_qualifier_pushdown, has_window,
    projection_columns, qualifier_filters_for_stmt, run_select_without_from_output,
    run_single_foreign_select_output, run_single_table_select_output, BTreeSet, ColumnPrune,
    ComputePlan, CteScope, Engine, QueryBlockPlan, QueryOutput, QueryOutputMode, SQLError,
    SQLParam, ScalarExpr, SourcePlan,
};

pub(in crate::sql) fn run_query_block_with_prepared_exists_output(
    engine: &Engine,
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let Some(from) = stmt.from.as_ref() else {
        return run_select_without_from_output(engine, block, stmt, params, ctes, output_mode);
    };

    // Set-op branches, CTEs, and derived-table bodies still need the same
    // search-aware single-table physical access path as top-level queries;
    // otherwise registry-backed predicates such as
    // `pool_positive_evidence(bayesian_match(...), knn_match(...))` fall
    // through to scalar expression evaluation.
    if let SourcePlan::Table { name, alias } = from {
        let foreign_table = engine
            .foreign_table(name)
            .map_err(|err| SQLError::Internal(format!("resolve foreign table `{name}`: {err}")))?;
        if alias.is_none() && foreign_table.is_some() {
            return run_single_foreign_select_output(
                engine,
                name,
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
        if alias.is_none() && !is_virtual {
            return run_single_table_select_output(
                engine,
                name,
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
    let operator = crate::sql::from_rows::build_join_operator_with_ctes(
        engine,
        from,
        params,
        ctes,
        column_prune.as_ref(),
        qualifier_filters.as_ref(),
    )?;
    let physical_filter =
        final_filter_after_qualifier_pushdown(engine, stmt, from, qualifier_filters.as_ref());

    let columns = if matches!(block.compute, ComputePlan::Project) {
        expand_from_star_columns(
            engine,
            projection_columns(&stmt.projections),
            &stmt.projections,
            from,
        )
    } else {
        projection_columns(&stmt.projections)
    };
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
    if has_window(&stmt.projections)
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

pub(in crate::sql) fn collect_from_qualifiers(from: &SourcePlan, out: &mut Vec<String>) {
    match from {
        SourcePlan::Table { name, alias } => {
            out.push(alias.clone().unwrap_or_else(|| name.clone()));
        }
        SourcePlan::Join { left, right, .. } => {
            collect_from_qualifiers(left, out);
            collect_from_qualifiers(right, out);
        }
        SourcePlan::Values { alias, .. }
        | SourcePlan::Function { alias, .. }
        | SourcePlan::Subquery { alias, .. } => {
            if let Some(alias) = alias {
                out.push(alias.clone());
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
        ScalarExpr::Star
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => {
            *valid = false;
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
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
