//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Qualifier and CTE output-filter pushdown and specialization.

use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::semantics::volatility::{
    expr_contains_volatile_function, query_contains_volatile_function,
};
use uqa_sql::semantics::{
    expr_contains_subquery, expr_qualifiers, flatten_and_filter_parts, from_qualifier_set,
    projection_columns, qualify_unqualified_columns, TABLE_OID_COLUMN, XMIN_COLUMN,
};
use uqa_sql::{
    plan::{
        source_projection::QualifierFilters, ProjectionPlan, QueryBlockPlan, QueryPlan,
        RelationalPlan, SourcePlan, UnifiedPlan,
    },
    SQLError, ScalarExpr,
};
pub mod context;
use context::{FilterPushdownContext, FilterPushdownScope};
mod output_boundary;
mod source_columns;

use source_columns::{ColumnOwners, SourceColumns};

pub fn qualifier_filters_for_stmt(
    context: FilterPushdownContext<'_>,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    scope: FilterPushdownScope<'_>,
) -> Result<Option<QualifierFilters>, SQLError> {
    // Column ownership is not fixed until the native source supplies its descriptor.
    if !stmt.deferred_outer_rewrites.is_empty() {
        return Ok(None);
    }
    let Some(filter) = stmt.r#where.as_ref() else {
        return Ok(None);
    };
    // Pushdown is decided per conjunct, not for the WHERE clause as a whole.
    // `qualifier_filter_for_part` already refuses any part containing a
    // subquery, and `final_filter_after_qualifier_pushdown` recomputes the
    // residual with that same rule, so the two stay consistent.
    //
    // Bailing out here whenever the clause mentioned a subquery anywhere also
    // abandoned conjuncts that push safely. A retrieval predicate then stayed
    // in the residual scalar filter, where registered retrieval functions
    // cannot be evaluated at all, so `text_match(...) AND EXISTS (...)` failed
    // with "scalar evaluation of `text_match` is not supported".
    let from_quals = from_qualifier_set(from);
    if from_quals.is_empty() {
        return Ok(None);
    }
    let columns = SourceColumns::new(scope, from, &from_quals)?;
    let nullable_qualifiers = outer_join_nullable_qualifiers(from);
    let mut filters = QualifierFilters::new();
    for part in flatten_and_filter_parts(filter) {
        if let Some((qualifier, filter)) =
            qualifier_filter_for_part(context, part, &from_quals, &columns, &stmt.subqueries)
        {
            if !nullable_qualifiers.contains(&qualifier) {
                filters.entry(qualifier).or_default().push(filter);
            }
        } else if qualifier_filter_elision_safe(from) {
            for (qualifier, filter) in derived_disjunctive_qualifier_filters(
                context,
                part,
                &from_quals,
                &columns,
                &stmt.subqueries,
            ) {
                if !nullable_qualifiers.contains(&qualifier) {
                    filters.entry(qualifier).or_default().push(filter);
                }
            }
        }
    }
    Ok((!filters.is_empty()).then_some(filters))
}

pub use uqa_sql::semantics::outer_join_nullable_qualifiers;

/// Project a multi-relation disjunction onto each relation as a necessary
/// predicate. For `(A1 AND B1) OR (A2 AND B2)`, `A1 OR A2` is safe to apply to
/// A before the join, and likewise for B. The original disjunction remains as
/// a residual filter, so this never turns the necessary predicate into a
/// sufficient one.
fn derived_disjunctive_qualifier_filters(
    context: FilterPushdownContext<'_>,
    part: &ScalarExpr,
    from_quals: &BTreeSet<String>,
    columns: &SourceColumns,
    subqueries: &[QueryPlan],
) -> Vec<(String, ScalarExpr)> {
    let ScalarExpr::Or(disjuncts) = part else {
        return Vec::new();
    };
    if disjuncts.len() < 2
        || expr_contains_subquery(part)
        || expr_contains_volatile_function(context.volatility, part)
    {
        return Vec::new();
    }

    let mut derived = Vec::new();
    for qualifier in from_quals {
        let mut projected_disjuncts = Vec::with_capacity(disjuncts.len());
        let mut complete = true;
        for disjunct in disjuncts {
            let local = flatten_and_filter_parts(disjunct)
                .into_iter()
                .filter_map(|conjunct| {
                    let (owner, predicate) = qualifier_filter_for_part(
                        context, conjunct, from_quals, columns, subqueries,
                    )?;
                    (owner == *qualifier).then_some(predicate)
                })
                .collect();
            let Some(projected) = combine_filter_parts(local) else {
                complete = false;
                break;
            };
            projected_disjuncts.push(projected);
        }
        if complete {
            let predicate = match projected_disjuncts.len() {
                0 => continue,
                1 => projected_disjuncts.pop().expect("one projected disjunct"),
                _ => ScalarExpr::Or(projected_disjuncts),
            };
            derived.push((qualifier.clone(), predicate));
        }
    }
    derived
}

fn qualifier_filter_for_part(
    context: FilterPushdownContext<'_>,
    part: &ScalarExpr,
    from_quals: &BTreeSet<String>,
    columns: &SourceColumns,
    subqueries: &[QueryPlan],
) -> Option<(String, ScalarExpr)> {
    if expression_contains_internal_column(part) {
        return None;
    }
    let contains_subquery = expr_contains_subquery(part);
    let unsafe_subquery = contains_subquery
        && (!subqueries_are_uncorrelated_and_stable(context, part, subqueries)
            || outer_expression_contains_volatile_function(context, part));
    let unsafe_scalar = !contains_subquery
        && expr_contains_volatile_function(context.volatility, part)
        && !crate::optimizer::contains_retrieval(part);
    if unsafe_subquery || unsafe_scalar {
        return None;
    }
    let qualifiers = expr_qualifiers(part);
    let unqualified = unqualified_column_names(part);
    // Every unqualified name must read a column of the receiving relation. A name it does not expose, such as the relation's own name used as a whole-row value, an outer column or another relation's column, keeps the conjunct in the residual filter.
    let reads_columns_of = |qualifier: &str| {
        unqualified
            .iter()
            .all(|column| columns.reads_column_of(column, qualifier))
    };
    let placed = |qualifier: &str| {
        Some((
            qualifier.to_string(),
            qualify_unqualified_columns(part, qualifier),
        ))
    };
    match qualifiers.len() {
        1 => {
            let qualifier = qualifiers.iter().next()?;
            (from_quals.contains(qualifier) && reads_columns_of(qualifier))
                .then(|| placed(qualifier))
                .flatten()
        }
        0 if unqualified.is_empty() => crate::optimizer::contains_retrieval(part)
            .then(|| columns.only_relation())
            .flatten()
            .and_then(placed),
        0 => {
            if let Some(qualifier) = unique_unqualified_column_owner(part, columns.owners()) {
                if from_quals.contains(qualifier) {
                    return placed(qualifier);
                }
            }
            let qualifier = columns.only_relation()?;
            reads_columns_of(qualifier)
                .then(|| placed(qualifier))
                .flatten()
        }
        _ => None,
    }
}

/// The distinct names of a conjunct's unqualified column references.
fn unqualified_column_names(expression: &ScalarExpr) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    expression.visit(&mut |node| {
        if let ScalarExpr::Column(name) = node {
            names.insert(name.to_string());
        }
    });
    names
}

fn expression_contains_internal_column(expression: &ScalarExpr) -> bool {
    let mut found = false;
    expression.visit(&mut |node| {
        found |= matches!(node, ScalarExpr::InternalColumn(_));
    });
    found
}

pub fn final_filter_after_qualifier_pushdown(
    context: FilterPushdownContext<'_>,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filters: Option<&QualifierFilters>,
    scope: FilterPushdownScope<'_>,
) -> Result<Option<ScalarExpr>, SQLError> {
    let Some(filter) = stmt.r#where.as_ref() else {
        return Ok(None);
    };
    if !qualifier_filter_elision_safe(from) {
        return Ok(Some(filter.clone()));
    }
    let from_quals = from_qualifier_set(from);
    let columns = SourceColumns::new(scope, from, &from_quals)?;
    let mut guaranteed = Vec::new();
    collect_guaranteed_join_filters(from, &mut guaranteed);
    let residual: Vec<ScalarExpr> = flatten_and_filter_parts(filter)
        .into_iter()
        .filter(|part| {
            let pushed = filters.is_some()
                && qualifier_filter_for_part(
                    context,
                    part,
                    &from_quals,
                    &columns,
                    &stmt.subqueries,
                )
                .is_some();
            let guaranteed_by_join = !expr_contains_volatile_function(context.volatility, part)
                && guaranteed.contains(part);
            !pushed && !guaranteed_by_join
        })
        .cloned()
        .collect();
    Ok(combine_filter_parts(residual))
}

pub fn qualifier_filter_elision_safe(from: &SourcePlan) -> bool {
    match from {
        SourcePlan::RetainedRows(_) => true,
        SourcePlan::Join {
            left,
            right,
            kind,
            alias,
            ..
        } => {
            alias.is_none()
                && matches!(
                    kind,
                    uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross
                )
                && qualifier_filter_elision_safe(left)
                && qualifier_filter_elision_safe(right)
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => true,
    }
}

pub fn combine_filter_parts(mut parts: Vec<ScalarExpr>) -> Option<ScalarExpr> {
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(ScalarExpr::And(parts)),
    }
}

/// Find predicates on a directly referenced CTE output. The predicate remains
/// on the consumer and is duplicated into the CTE only when that CTE has one
/// reference in this query block. This makes the rewrite semantics-preserving
/// for shared CTE materializations.
pub fn cte_output_filters(
    context: FilterPushdownContext<'_>,
    plan: &QueryPlan,
    scope: FilterPushdownScope<'_>,
) -> Result<BTreeMap<String, (String, ScalarExpr)>, SQLError> {
    let RelationalPlan::QueryBlock(block) = &plan.root else {
        return Ok(BTreeMap::new());
    };
    let (Some(from), Some(filter)) = (block.from.as_ref(), block.r#where.as_ref()) else {
        return Ok(BTreeMap::new());
    };
    if expr_contains_subquery(filter) || expr_contains_volatile_function(context.volatility, filter)
    {
        return Ok(BTreeMap::new());
    }

    let cte_names: BTreeSet<&str> = plan.ctes.iter().map(|cte| cte.name.as_str()).collect();
    let mut references: BTreeMap<String, Vec<String>> = BTreeMap::new();
    collect_cte_source_references(from, &cte_names, &mut references);
    let qualifier_to_cte: BTreeMap<String, String> = references
        .into_iter()
        .filter_map(|(cte, qualifiers)| {
            (qualifiers.len() == 1).then(|| (qualifiers[0].clone(), cte))
        })
        .collect();
    if qualifier_to_cte.is_empty() {
        return Ok(BTreeMap::new());
    }

    let from_qualifiers = from_qualifier_set(from);
    let columns = SourceColumns::new(scope, from, &from_qualifiers)?;
    let mut grouped: BTreeMap<String, (String, Vec<ScalarExpr>)> = BTreeMap::new();
    for part in flatten_and_filter_parts(filter) {
        let Some((qualifier, predicate)) =
            qualifier_filter_for_part(context, part, &from_qualifiers, &columns, &block.subqueries)
        else {
            continue;
        };
        let Some(cte_name) = qualifier_to_cte.get(&qualifier) else {
            continue;
        };
        let entry = grouped
            .entry(cte_name.clone())
            .or_insert_with(|| (qualifier, Vec::new()));
        entry.1.push(predicate);
    }

    Ok(grouped
        .into_iter()
        .filter_map(|(name, (qualifier, predicates))| {
            combine_filter_parts(predicates).map(|predicate| (name, (qualifier, predicate)))
        })
        .collect())
}

mod subqueries;
pub use subqueries::collect_subquery_ids;
use subqueries::{
    outer_expression_contains_volatile_function, subqueries_are_uncorrelated_and_stable,
    unique_unqualified_column_owner,
};
fn collect_pushdown_outer_columns(expression: &ScalarExpr, output: &mut BTreeSet<String>) -> bool {
    match expression {
        ScalarExpr::Column(column) => {
            output.insert(column.to_string());
            true
        }
        ScalarExpr::QualifiedColumn { column, .. } => {
            output.insert(column.to_string());
            true
        }
        ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => true,
        ScalarExpr::InSubquery { expr, .. } => collect_pushdown_outer_columns(expr, output),
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items
            .iter()
            .all(|item| collect_pushdown_outer_columns(item, output)),
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter()
                .all(|argument| collect_pushdown_outer_columns(argument, output))
                && order_by
                    .iter()
                    .all(|order| collect_pushdown_outer_columns(&order.expr, output))
                && filter
                    .as_deref()
                    .is_none_or(|filter| collect_pushdown_outer_columns(filter, output))
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_pushdown_outer_columns(lhs, output)
                && collect_pushdown_outer_columns(rhs, output)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => collect_pushdown_outer_columns(inner, output),
        ScalarExpr::Between { expr, low, high } => {
            collect_pushdown_outer_columns(expr, output)
                && collect_pushdown_outer_columns(low, output)
                && collect_pushdown_outer_columns(high, output)
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_pushdown_outer_columns(expr, output)
                && list
                    .iter()
                    .all(|item| collect_pushdown_outer_columns(item, output))
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref()
                .is_none_or(|base| collect_pushdown_outer_columns(base, output))
                && when.iter().all(|(condition, result)| {
                    collect_pushdown_outer_columns(condition, output)
                        && collect_pushdown_outer_columns(result, output)
                })
                && else_branch
                    .as_deref()
                    .is_none_or(|branch| collect_pushdown_outer_columns(branch, output))
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::WindowCall { .. } => false,
    }
}

fn collect_guaranteed_join_filters<'a>(source: &'a SourcePlan, filters: &mut Vec<&'a ScalarExpr>) {
    if let SourcePlan::Join {
        left,
        right,
        kind: uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross,
        on,
        ..
    } = source
    {
        collect_guaranteed_join_filters(left, filters);
        collect_guaranteed_join_filters(right, filters);
        if let Some(on) = on {
            filters.extend(flatten_and_filter_parts(on));
        }
    }
}

pub fn collect_cte_source_references(
    source: &SourcePlan,
    cte_names: &BTreeSet<&str>,
    references: &mut BTreeMap<String, Vec<String>>,
) {
    match source {
        SourcePlan::RetainedRows(_) => {}
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            ..
        } => {
            if let Some(name) = uqa_sql::semantics::cte_reference_name(name)
                .filter(|name| cte_names.contains(name.as_str()))
            {
                references
                    .entry(name)
                    .or_default()
                    .push(alias.clone().unwrap_or_else(|| qualifier.clone()));
            }
        }
        SourcePlan::Join { left, right, .. } => {
            collect_cte_source_references(left, cte_names, references);
            collect_cte_source_references(right, cte_names, references);
        }
        SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => {}
    }
}

/// Specialize a physical query plan with a predicate on its output columns.
/// A returned plan applies the complete predicate below the output boundary under the rewrite guards; the caller retains the original predicate when specialization is declined.
pub fn push_output_filter_into_query_plan(
    context: FilterPushdownContext<'_>,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Result<Option<QueryPlan>, SQLError> {
    if query_contains_volatile_function(context.volatility, plan)? {
        return Ok(None);
    }
    push_derived_output_filter(context, plan, qualifier, filter, output_columns_override)
}

/// Place an ordinary source predicate before its outputs. Unobserved volatile outputs do not prevent a pure predicate on other columns from selecting their input rows.
pub fn push_derived_output_filter(
    context: FilterPushdownContext<'_>,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Result<Option<QueryPlan>, SQLError> {
    let Some(mut columns) = plan
        .analyzed_output
        .as_ref()
        .filter(|schema| !schema.columns_are_open(None))
        .map(|schema| schema.columns().to_vec())
        .or_else(|| query_plan_output_columns(plan))
    else {
        return Ok(None);
    };
    if let Some(aliases) = output_columns_override {
        if aliases.len() > columns.len() {
            return Ok(None);
        }
        for (column, alias) in columns.iter_mut().zip(aliases) {
            column.clone_from(alias);
        }
    }
    let output_columns_override = Some(columns.as_slice());
    if expr_contains_subquery(filter)
        || expr_contains_volatile_function(context.volatility, filter)
        || !output_boundary::preserves_set_types(plan, qualifier, filter, output_columns_override)
    {
        return Ok(None);
    }
    let Some(specialized) =
        specialize_query_output_filter(context, plan, qualifier, filter, output_columns_override)
    else {
        return Ok(None);
    };
    match (context.optimizer)(UnifiedPlan::Query(Box::new(specialized)))? {
        UnifiedPlan::Query(plan) => Ok(Some(*plan)),
        UnifiedPlan::Command(_) => Err(SQLError::Internal(
            "query optimizer changed a query into a command plan".into(),
        )),
    }
}

pub fn specialize_query_output_filter(
    context: FilterPushdownContext<'_>,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<QueryPlan> {
    let mut specialized = plan.clone();
    specialize_relational_output_filter(
        context,
        &mut specialized.root,
        qualifier,
        filter,
        output_columns_override,
    )?;
    Some(specialized)
}

pub fn specialize_relational_output_filter(
    context: FilterPushdownContext<'_>,
    root: &mut RelationalPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<()> {
    match root {
        RelationalPlan::QueryBlock(block) => specialize_query_block_output_filter(
            context,
            block,
            qualifier,
            filter,
            output_columns_override,
        ),
        RelationalPlan::SetOp {
            left,
            right,
            limit,
            offset,
            ..
        } => {
            if limit.is_some() || offset.is_some() {
                return None;
            }
            let output_columns = match output_columns_override {
                Some(columns) => columns.to_vec(),
                None => query_plan_output_columns(left)?,
            };
            let specialized_left = specialize_query_output_filter(
                context,
                left,
                qualifier,
                filter,
                Some(&output_columns),
            )?;
            let specialized_right = specialize_query_output_filter(
                context,
                right,
                qualifier,
                filter,
                Some(&output_columns),
            )?;
            **left = specialized_left;
            **right = specialized_right;
            Some(())
        }
        RelationalPlan::Values { .. } => None,
    }
}

pub use uqa_sql::semantics::query_plan_output_columns;

pub fn specialize_query_block_output_filter(
    context: FilterPushdownContext<'_>,
    block: &mut QueryBlockPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<()> {
    let rewritten = derived_output_predicate(
        context.correlation.routines,
        context.volatility,
        block,
        qualifier,
        filter,
        output_columns_override,
        false,
    )?;
    block.r#where = match block.r#where.take() {
        Some(existing) => Some(ScalarExpr::And(vec![existing, rewritten])),
        None => Some(rewritten),
    };
    Some(())
}

/// Select a legal producer predicate. Query slots are retained only for a caller that transfers their arena without duplicating their execution identity.
pub(crate) fn derived_output_predicate(
    routines: &dyn uqa_sql::routines::RoutineResolution,
    volatility: &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    block: &QueryBlockPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
    retain_query_slots: bool,
) -> Option<ScalarExpr> {
    if block.limit.is_some() || block.offset.is_some() || !block.grouping_sets.is_empty() {
        return None;
    }

    let mut output_columns = projection_columns(&block.projections);
    if let Some(aliases) = output_columns_override {
        if aliases.len() > output_columns.len() {
            return None;
        }
        for (column, alias) in output_columns.iter_mut().zip(aliases) {
            column.clone_from(alias);
        }
    }
    let mut used = BTreeSet::new();
    let rewritten = rewrite_output_filter_inputs(
        filter,
        qualifier,
        &output_columns,
        &block.projections,
        &mut used,
        retain_query_slots,
    )?;
    if used.is_empty() {
        return None;
    }

    for index in &used {
        let expression = &block.projections[*index].expr;
        if matches!(expression, ScalarExpr::Star)
            || expression.contains_window()
            || expr_contains_subquery(expression)
            || expr_contains_volatile_function(volatility, expression)
            || uqa_sql::semantics::sets::static_setness::expression_may_return_set_statically(
                routines, expression,
            )
        {
            return None;
        }
        if !output_boundary::preserves_groups(block, expression) {
            return None;
        }
    }

    Some(rewritten)
}

pub fn rewrite_output_filter(
    expression: &ScalarExpr,
    qualifier: &str,
    output_columns: &[String],
    projections: &[ProjectionPlan],
    used: &mut BTreeSet<usize>,
) -> Option<ScalarExpr> {
    rewrite_output_filter_inputs(
        expression,
        qualifier,
        output_columns,
        projections,
        used,
        false,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
fn rewrite_output_filter_inputs(
    expression: &ScalarExpr,
    qualifier: &str,
    output_columns: &[String],
    projections: &[ProjectionPlan],
    used: &mut BTreeSet<usize>,
    retain_query_slots: bool,
) -> Option<ScalarExpr> {
    let map_column = |column: &str, used: &mut BTreeSet<usize>| {
        let index = output_columns
            .iter()
            .position(|candidate| candidate == column)?;
        used.insert(index);
        Some(projections[index].expr.clone())
    };
    let recur = |expression: &ScalarExpr, used: &mut BTreeSet<usize>| {
        rewrite_output_filter_inputs(
            expression,
            qualifier,
            output_columns,
            projections,
            used,
            retain_query_slots,
        )
    };

    Some(match expression {
        ScalarExpr::ScalarSubquery(_) | ScalarExpr::Exists { .. } if retain_query_slots => {
            expression.clone()
        }
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } if retain_query_slots => ScalarExpr::InSubquery {
            expr: Box::new(recur(expr, used)?),
            subquery: *subquery,
            negated: *negated,
        },
        ScalarExpr::Column(column) => map_column(column, used)?,
        ScalarExpr::QualifiedColumn {
            qualifier: expression_qualifier,
            column,
            ..
        } if expression_qualifier == qualifier => map_column(column, used)?,
        ScalarExpr::Default
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => return None,
        ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_) => expression.clone(),
        ScalarExpr::Array(items) => ScalarExpr::Array(
            items
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
        ),
        ScalarExpr::Row(items) => ScalarExpr::Row(
            items
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
        ),
        ScalarExpr::CompositeRow {
            items,
            binding,
            bound_type,
        } => ScalarExpr::CompositeRow {
            items: items
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
            binding: binding.clone(),
            bound_type: bound_type.clone(),
        },
        ScalarExpr::Func {
            order_syntax,
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
            order_syntax: *order_syntax,
            name: name.clone(),
            binding: binding.clone(),
            args: args
                .iter()
                .map(|arg| recur(arg, used))
                .collect::<Option<Vec<_>>>()?,
            distinct: *distinct,
            order_by: order_by
                .iter()
                .map(|order| {
                    Some(uqa_sql::ScalarOrder {
                        expr: recur(&order.expr, used)?,
                        descending: order.descending,
                        nulls: order.nulls,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
            filter: match filter.as_deref() {
                Some(filter) => Some(Box::new(recur(filter, used)?)),
                None => None,
            },
        },
        ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(recur(lhs, used)?),
            rhs: Box::new(recur(rhs, used)?),
        },
        ScalarExpr::Not(inner) => ScalarExpr::Not(Box::new(recur(inner, used)?)),
        ScalarExpr::UnaryMinus(inner) => ScalarExpr::UnaryMinus(Box::new(recur(inner, used)?)),
        ScalarExpr::And(items) => ScalarExpr::And(
            items
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
        ),
        ScalarExpr::Or(items) => ScalarExpr::Or(
            items
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
        ),
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(recur(expr, used)?),
            negated: *negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(recur(expr, used)?),
            low: Box::new(recur(low, used)?),
            high: Box::new(recur(high, used)?),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(recur(expr, used)?),
            list: list
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
            negated: *negated,
        },
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: match base.as_deref() {
                Some(base) => Some(Box::new(recur(base, used)?)),
                None => None,
            },
            when: when
                .iter()
                .map(|(condition, result)| Some((recur(condition, used)?, recur(result, used)?)))
                .collect::<Option<Vec<_>>>()?,
            else_branch: match else_branch.as_deref() {
                Some(branch) => Some(Box::new(recur(branch, used)?)),
                None => None,
            },
        },
        ScalarExpr::Collated {
            expr,
            ty,
            collation,
        } => ScalarExpr::Collated {
            expr: Box::new(recur(expr, used)?),
            ty: ty.clone(),
            collation: *collation,
        },
        ScalarExpr::Cast { expr, ty, implicit } => ScalarExpr::Cast {
            implicit: *implicit,
            expr: Box::new(recur(expr, used)?),
            ty: ty.clone(),
        },
    })
}

#[cfg(test)]
mod tests;
