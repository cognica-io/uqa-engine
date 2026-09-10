//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Required source columns and row metadata for physical query projection.

use std::collections::BTreeSet;
use uqa_sql::{
    catalog::{analysis::AnalysisCatalog, resolution::RelationNameResolution},
    plan::{
        source_projection::{ColumnPrune, SourceProjection},
        QueryBlockPlan, SourcePlan,
    },
    semantics::volatility::{expr_contains_volatile_function, VolatilityCatalog},
    semantics::{
        expr_contains_subquery, DOC_ID_COLUMN, META_DOC_ID_COLUMN, META_QUALIFIER,
        META_SCORE_COLUMN, SCORE_COLUMN,
    },
    SQLError, ScalarExpr,
};

#[derive(Clone, Copy)]
pub struct ColumnPruneContext<'a> {
    pub catalog: &'a dyn AnalysisCatalog,
    pub resolution: &'a RelationNameResolution,
    pub volatility: &'a dyn VolatilityCatalog,
    pub is_visible_cte: &'a dyn Fn(&str) -> bool,
}

fn has_window(projections: &[uqa_sql::plan::ProjectionPlan]) -> bool {
    projections
        .iter()
        .any(|projection| uqa_sql::semantics::windows::expr_has_window(&projection.expr))
}

pub fn column_prune_for_stmt(
    context: ColumnPruneContext<'_>,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
) -> Result<Option<ColumnPrune>, SQLError> {
    column_prune_for_stmt_with_filter(context, stmt, from, stmt.r#where.as_ref())
}

/// Compute the document projection for `stmt` while treating `filter` as the
/// only predicate that remains to be evaluated by the relational pipeline.
/// Accelerated retrieval consumes its search predicate before constructing a
/// scored document source, so its field
/// arguments are index dependencies rather than row-materialization
/// dependencies. Callers that have executed retrieval pass only the residual
/// predicate here; ordinary scans retain the statement's original `WHERE` via
/// [`column_prune_for_stmt`].
pub fn column_prune_for_stmt_with_filter(
    context: ColumnPruneContext<'_>,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filter: Option<&ScalarExpr>,
) -> Result<Option<ColumnPrune>, SQLError> {
    let catalog = context.catalog;
    let resolution = context.resolution;
    let requires_full_projection = source_contains_join_alias(from)
        || has_window(&stmt.projections)
        || stmt.projections.iter().any(|projection| {
            matches!(projection.expr, ScalarExpr::Star)
                || expr_contains_subquery(&projection.expr)
                || expr_contains_volatile_function(context.volatility, &projection.expr)
        });

    let mut qualifiers = Vec::new();
    collect_from_qualifiers(from, &mut qualifiers);
    if qualifiers.is_empty() {
        return Ok(None);
    }

    let metadata_binding =
        single_local_table_metadata_binding(catalog, resolution, from, context.is_visible_cte)?;
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
    collect_join_binding_prune_columns(catalog, resolution, from, &mut prune)?;
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
    catalog: &dyn AnalysisCatalog,
    resolution: &RelationNameResolution,
    source: &SourcePlan,
    is_visible_cte: &dyn Fn(&str) -> bool,
) -> Result<Option<LocalTableMetadataBinding>, SQLError> {
    fn collect(
        catalog: &dyn AnalysisCatalog,
        resolution: &RelationNameResolution,
        source: &SourcePlan,
        is_visible_cte: &dyn Fn(&str) -> bool,
        relations: &mut BTreeSet<(String, String)>,
    ) -> Result<(), SQLError> {
        match source {
            SourcePlan::Table {
                name,
                qualifier,
                alias,
                ..
            } => {
                if !is_visible_cte(name) {
                    if let Some(name) = catalog.table_name_resolved(resolution, name)? {
                        relations.insert((alias.as_deref().unwrap_or(qualifier).to_string(), name));
                    }
                }
            }
            SourcePlan::Join { left, right, .. } => {
                collect(catalog, resolution, left, is_visible_cte, relations)?;
                collect(catalog, resolution, right, is_visible_cte, relations)?;
            }
            SourcePlan::Values { .. }
            | SourcePlan::Function { .. }
            | SourcePlan::FunctionGroup { .. }
            | SourcePlan::Subquery { .. } => {}
        }
        Ok(())
    }
    if source_contains_join_alias(source) {
        return Ok(None);
    }
    let mut relations = BTreeSet::new();
    collect(catalog, resolution, source, is_visible_cte, &mut relations)?;
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
    catalog: &dyn AnalysisCatalog,
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
    catalog: &dyn AnalysisCatalog,
    resolution: &RelationNameResolution,
    source: &SourcePlan,
    prune: &mut ColumnPrune,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            column_aliases,
            ..
        } => {
            let qualifier = alias.as_deref().unwrap_or(qualifier);
            match catalog.table_resolved(resolution, name)? {
                Some(table) => {
                    if let Some(columns) = prune.get_mut(qualifier) {
                        columns.extend(table.columns.iter().enumerate().map(
                            |(position, column)| {
                                column_aliases
                                    .get(position)
                                    .cloned()
                                    .unwrap_or_else(|| column.name.clone())
                            },
                        ));
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
            let routine_columns = uqa_sql::binding::catalog_sources::user_function_output_columns(
                catalog, resolution, name,
            )?;
            columns.extend(routine_columns.map_or_else(
                || {
                    uqa_sql::semantics::table_function_empty_schema(
                        name,
                        output_name,
                        alias.as_deref(),
                        column_aliases,
                        args.len(),
                        *ordinality,
                    )
                },
                |base| {
                    uqa_sql::semantics::apply_table_function_aliases(
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
                    uqa_sql::binding::catalog_sources::user_function_output_columns(
                        catalog,
                        resolution,
                        &function.name,
                    )?;
                group_columns.extend(routine_columns.map_or_else(
                    || {
                        uqa_sql::semantics::table_function_empty_schema(
                            &function.name,
                            &function.output_name,
                            None,
                            &function.column_aliases,
                            function.args.len(),
                            false,
                        )
                    },
                    |base| {
                        uqa_sql::semantics::apply_table_function_aliases(
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
                columns.extend(
                    uqa_sql::semantics::query_plan_output_columns(body).unwrap_or_default(),
                );
            } else {
                columns.extend(column_aliases.iter().cloned());
            }
        }
    }
    Ok(())
}

pub use uqa_sql::semantics::collect_from_qualifiers;

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
            for qualifier in scope.qualifiers {
                if qualifier.eq_ignore_ascii_case(column) {
                    let Some(source) = prune.get_mut(qualifier) else {
                        *valid = false;
                        return;
                    };
                    source.retain_all();
                }
            }
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
        ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. } | ScalarExpr::Param(_) => {}
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
                    if let uqa_sql::ScalarFrameBound::Preceding(expression)
                    | uqa_sql::ScalarFrameBound::Following(expression) = bound
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
