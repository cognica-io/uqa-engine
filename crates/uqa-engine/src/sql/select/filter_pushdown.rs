//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Qualifier and CTE output-filter pushdown and specialization.

use super::{
    expr_contains_function, expr_contains_subquery, expr_contains_volatile_function,
    expr_has_unqualified_column, expr_qualifiers, flatten_and_filter_parts, from_qualifier_set,
    optimize_engine_plan, projection_columns, qualify_unqualified_columns,
    query_contains_volatile_function, BTreeMap, BTreeSet, ComputePlan, Engine, ProjectionPlan,
    QualifierFilters, QueryBlockPlan, QueryPlan, RelationalPlan, SQLError, ScalarExpr, SourcePlan,
    UnifiedPlan,
};

type ColumnOwners = BTreeMap<String, Option<String>>;

pub(in crate::sql) fn qualifier_filters_for_stmt(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
) -> Option<QualifierFilters> {
    let filter = stmt.r#where.as_ref()?;
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
        return None;
    }
    let single_qualifier = (from_quals.len() == 1)
        .then(|| from_quals.iter().next().cloned())
        .flatten();
    let column_owners = source_column_owners(engine, from);
    let mut filters = QualifierFilters::new();
    for part in flatten_and_filter_parts(filter) {
        if let Some((qualifier, filter)) = qualifier_filter_for_part(
            engine,
            part,
            &from_quals,
            single_qualifier.as_deref(),
            &column_owners,
            &stmt.subqueries,
        ) {
            filters.entry(qualifier).or_default().push(filter);
        } else if qualifier_filter_elision_safe(from) {
            for (qualifier, filter) in derived_disjunctive_qualifier_filters(
                engine,
                part,
                &from_quals,
                single_qualifier.as_deref(),
                &column_owners,
                &stmt.subqueries,
            ) {
                filters.entry(qualifier).or_default().push(filter);
            }
        }
    }
    (!filters.is_empty()).then_some(filters)
}

/// Project a multi-relation disjunction onto each relation as a necessary
/// predicate. For `(A1 AND B1) OR (A2 AND B2)`, `A1 OR A2` is safe to apply to
/// A before the join, and likewise for B. The original disjunction remains as
/// a residual filter, so this never turns the necessary predicate into a
/// sufficient one.
fn derived_disjunctive_qualifier_filters(
    engine: &Engine,
    part: &ScalarExpr,
    from_quals: &BTreeSet<String>,
    single_qualifier: Option<&str>,
    column_owners: &ColumnOwners,
    subqueries: &[QueryPlan],
) -> Vec<(String, ScalarExpr)> {
    let ScalarExpr::Or(disjuncts) = part else {
        return Vec::new();
    };
    if disjuncts.len() < 2
        || expr_contains_subquery(part)
        || expr_contains_volatile_function(engine, part)
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
                        engine,
                        conjunct,
                        from_quals,
                        single_qualifier,
                        column_owners,
                        subqueries,
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
    engine: &Engine,
    part: &ScalarExpr,
    from_quals: &BTreeSet<String>,
    single_qualifier: Option<&str>,
    column_owners: &ColumnOwners,
    subqueries: &[QueryPlan],
) -> Option<(String, ScalarExpr)> {
    let contains_subquery = expr_contains_subquery(part);
    let unsafe_subquery = contains_subquery
        && (!subqueries_are_uncorrelated_and_stable(engine, part, subqueries)
            || outer_expression_contains_volatile_function(engine, part));
    let unsafe_scalar = !contains_subquery
        && expr_contains_volatile_function(engine, part)
        && !uqa_planner::optimizer::contains_retrieval(part);
    if unsafe_subquery || unsafe_scalar {
        return None;
    }
    let qualifiers = expr_qualifiers(part);
    let has_unqualified = expr_has_unqualified_column(part);
    if qualifiers.len() == 1 && (!has_unqualified || from_quals.len() == 1) {
        let qualifier = qualifiers.iter().next()?;
        if from_quals.contains(qualifier) {
            return Some((qualifier.clone(), part.clone()));
        }
    }
    if qualifiers.is_empty() && has_unqualified {
        if let Some(qualifier) = unique_unqualified_column_owner(part, column_owners) {
            if from_quals.contains(qualifier) {
                return Some((
                    qualifier.to_string(),
                    qualify_unqualified_columns(part, qualifier),
                ));
            }
        }
        if let Some(qualifier) = single_qualifier {
            return Some((
                qualifier.to_string(),
                qualify_unqualified_columns(part, qualifier),
            ));
        }
    }
    None
}

pub(in crate::sql) fn final_filter_after_qualifier_pushdown(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filters: Option<&QualifierFilters>,
) -> Option<ScalarExpr> {
    let filter = stmt.r#where.as_ref()?;
    if !qualifier_filter_elision_safe(from) {
        return Some(filter.clone());
    }
    let from_quals = from_qualifier_set(from);
    let single_qualifier = (from_quals.len() == 1)
        .then(|| from_quals.iter().next().cloned())
        .flatten();
    let column_owners = source_column_owners(engine, from);
    let mut guaranteed = Vec::new();
    collect_guaranteed_join_filters(from, &mut guaranteed);
    let residual: Vec<ScalarExpr> = flatten_and_filter_parts(filter)
        .into_iter()
        .filter(|part| {
            let pushed = filters.is_some()
                && qualifier_filter_for_part(
                    engine,
                    part,
                    &from_quals,
                    single_qualifier.as_deref(),
                    &column_owners,
                    &stmt.subqueries,
                )
                .is_some();
            let guaranteed_by_join =
                !expr_contains_volatile_function(engine, part) && guaranteed.contains(part);
            !pushed && !guaranteed_by_join
        })
        .cloned()
        .collect();
    combine_filter_parts(residual)
}

pub(in crate::sql) fn qualifier_filter_elision_safe(from: &SourcePlan) -> bool {
    match from {
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
        | SourcePlan::Subquery { .. } => true,
    }
}

pub(in crate::sql) fn combine_filter_parts(mut parts: Vec<ScalarExpr>) -> Option<ScalarExpr> {
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
pub(in crate::sql) fn cte_output_filters(
    engine: &Engine,
    plan: &QueryPlan,
) -> BTreeMap<String, (String, ScalarExpr)> {
    let RelationalPlan::QueryBlock(block) = &plan.root else {
        return BTreeMap::new();
    };
    let (Some(from), Some(filter)) = (block.from.as_ref(), block.r#where.as_ref()) else {
        return BTreeMap::new();
    };
    if expr_contains_subquery(filter) || expr_contains_volatile_function(engine, filter) {
        return BTreeMap::new();
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
        return BTreeMap::new();
    }

    let from_qualifiers = from_qualifier_set(from);
    let single_qualifier = (from_qualifiers.len() == 1)
        .then(|| from_qualifiers.iter().next().cloned())
        .flatten();
    let column_owners = source_column_owners(engine, from);
    let mut grouped: BTreeMap<String, (String, Vec<ScalarExpr>)> = BTreeMap::new();
    for part in flatten_and_filter_parts(filter) {
        let Some((qualifier, predicate)) = qualifier_filter_for_part(
            engine,
            part,
            &from_qualifiers,
            single_qualifier.as_deref(),
            &column_owners,
            &block.subqueries,
        ) else {
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

    grouped
        .into_iter()
        .filter_map(|(name, (qualifier, predicates))| {
            combine_filter_parts(predicates).map(|predicate| (name, (qualifier, predicate)))
        })
        .collect()
}

fn unique_unqualified_column_owner<'a>(
    expression: &ScalarExpr,
    owners: &'a ColumnOwners,
) -> Option<&'a str> {
    if !expr_qualifiers(expression).is_empty() {
        return None;
    }
    let mut columns = BTreeSet::new();
    if !collect_pushdown_outer_columns(expression, &mut columns) || columns.is_empty() {
        return None;
    }
    let mut owner = None;
    for column in columns {
        let candidate = owners.get(&column)?.as_deref()?;
        if owner.is_some_and(|owner| owner != candidate) {
            return None;
        }
        owner = Some(candidate);
    }
    owner
}

fn subqueries_are_uncorrelated_and_stable(
    engine: &Engine,
    expression: &ScalarExpr,
    subqueries: &[QueryPlan],
) -> bool {
    let mut referenced = BTreeSet::new();
    collect_subquery_ids(expression, &mut referenced);
    !referenced.is_empty()
        && referenced.into_iter().all(|id| {
            let Some(plan) = subqueries.get(id) else {
                return false;
            };
            matches!(
                crate::sql::correlation::query_depends_on_outer_row(engine, plan),
                Ok(false)
            ) && matches!(query_contains_volatile_function(engine, plan), Ok(false))
        })
}

fn outer_expression_contains_volatile_function(engine: &Engine, expression: &ScalarExpr) -> bool {
    if !expr_contains_subquery(expression) {
        return expr_contains_volatile_function(engine, expression);
    }
    match expression {
        ScalarExpr::ScalarSubquery(_) | ScalarExpr::Exists { .. } => false,
        ScalarExpr::InSubquery { expr, .. } => {
            outer_expression_contains_volatile_function(engine, expr)
        }
        ScalarExpr::Func {
            name,
            args,
            order_by,
            filter,
            ..
        } => {
            crate::sql::volatility::function_volatility(engine, name, args.len())
                == uqa_sql::ast::FunctionVolatility::Volatile
                || args
                    .iter()
                    .any(|expr| outer_expression_contains_volatile_function(engine, expr))
                || order_by
                    .iter()
                    .any(|order| outer_expression_contains_volatile_function(engine, &order.expr))
                || filter.as_deref().is_some_and(|filter| {
                    outer_expression_contains_volatile_function(engine, filter)
                })
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items
            .iter()
            .any(|item| outer_expression_contains_volatile_function(engine, item)),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            outer_expression_contains_volatile_function(engine, lhs)
                || outer_expression_contains_volatile_function(engine, rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            outer_expression_contains_volatile_function(engine, inner)
        }
        ScalarExpr::Between { expr, low, high } => {
            outer_expression_contains_volatile_function(engine, expr)
                || outer_expression_contains_volatile_function(engine, low)
                || outer_expression_contains_volatile_function(engine, high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            outer_expression_contains_volatile_function(engine, expr)
                || list
                    .iter()
                    .any(|item| outer_expression_contains_volatile_function(engine, item))
        }
        ScalarExpr::WindowCall { name, args, spec } => {
            crate::sql::volatility::function_volatility(engine, name, args.len())
                == uqa_sql::ast::FunctionVolatility::Volatile
                || args
                    .iter()
                    .any(|expr| outer_expression_contains_volatile_function(engine, expr))
                || spec
                    .partition_by
                    .iter()
                    .any(|expr| outer_expression_contains_volatile_function(engine, expr))
                || spec
                    .order_by
                    .iter()
                    .any(|order| outer_expression_contains_volatile_function(engine, &order.expr))
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_outer_expression_contains_volatile_function(engine, &frame.start)
                        || frame_bound_outer_expression_contains_volatile_function(
                            engine, &frame.end,
                        )
                })
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref()
                .is_some_and(|base| outer_expression_contains_volatile_function(engine, base))
                || when.iter().any(|(condition, result)| {
                    outer_expression_contains_volatile_function(engine, condition)
                        || outer_expression_contains_volatile_function(engine, result)
                })
                || else_branch.as_deref().is_some_and(|branch| {
                    outer_expression_contains_volatile_function(engine, branch)
                })
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_) => false,
    }
}

fn frame_bound_outer_expression_contains_volatile_function(
    engine: &Engine,
    bound: &uqa_execution::ScalarFrameBound,
) -> bool {
    match bound {
        uqa_execution::ScalarFrameBound::Preceding(expression)
        | uqa_execution::ScalarFrameBound::Following(expression) => {
            outer_expression_contains_volatile_function(engine, expression)
        }
        uqa_execution::ScalarFrameBound::UnboundedPreceding
        | uqa_execution::ScalarFrameBound::UnboundedFollowing
        | uqa_execution::ScalarFrameBound::CurrentRow => false,
    }
}

pub(in crate::sql) fn collect_subquery_ids(expression: &ScalarExpr, output: &mut BTreeSet<usize>) {
    match expression {
        ScalarExpr::ScalarSubquery(id) | ScalarExpr::Exists { subquery: id, .. } => {
            output.insert(*id);
        }
        ScalarExpr::InSubquery { expr, subquery, .. } => {
            collect_subquery_ids(expr, output);
            output.insert(*subquery);
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            for item in items {
                collect_subquery_ids(item, output);
            }
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_subquery_ids(argument, output);
            }
            for order in order_by {
                collect_subquery_ids(&order.expr, output);
            }
            if let Some(filter) = filter {
                collect_subquery_ids(filter, output);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_subquery_ids(lhs, output);
            collect_subquery_ids(rhs, output);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => collect_subquery_ids(inner, output),
        ScalarExpr::Between { expr, low, high } => {
            collect_subquery_ids(expr, output);
            collect_subquery_ids(low, output);
            collect_subquery_ids(high, output);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_subquery_ids(expr, output);
            for item in list {
                collect_subquery_ids(item, output);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_subquery_ids(argument, output);
            }
            for partition in &spec.partition_by {
                collect_subquery_ids(partition, output);
            }
            for order in &spec.order_by {
                collect_subquery_ids(&order.expr, output);
            }
            if let Some(frame) = &spec.frame {
                collect_frame_bound_subquery_ids(&frame.start, output);
                collect_frame_bound_subquery_ids(&frame.end, output);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_subquery_ids(base, output);
            }
            for (condition, result) in when {
                collect_subquery_ids(condition, output);
                collect_subquery_ids(result, output);
            }
            if let Some(branch) = else_branch {
                collect_subquery_ids(branch, output);
            }
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_) => {}
    }
}

fn collect_frame_bound_subquery_ids(
    bound: &uqa_execution::ScalarFrameBound,
    output: &mut BTreeSet<usize>,
) {
    match bound {
        uqa_execution::ScalarFrameBound::Preceding(expression)
        | uqa_execution::ScalarFrameBound::Following(expression) => {
            collect_subquery_ids(expression, output);
        }
        uqa_execution::ScalarFrameBound::UnboundedPreceding
        | uqa_execution::ScalarFrameBound::UnboundedFollowing
        | uqa_execution::ScalarFrameBound::CurrentRow => {}
    }
}

fn collect_pushdown_outer_columns(expression: &ScalarExpr, output: &mut BTreeSet<String>) -> bool {
    match expression {
        ScalarExpr::Column(column) => {
            output.insert(column.clone());
            true
        }
        ScalarExpr::QualifiedColumn { column, .. } => {
            output.insert(column.clone());
            true
        }
        ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => true,
        ScalarExpr::InSubquery { expr, .. } => collect_pushdown_outer_columns(expr, output),
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
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
        | ScalarExpr::Cast { expr: inner, .. } => collect_pushdown_outer_columns(inner, output),
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
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::WindowCall { .. } => false,
    }
}

fn source_column_owners(engine: &Engine, source: &SourcePlan) -> ColumnOwners {
    let mut owners = ColumnOwners::new();
    collect_source_column_owners(engine, source, &mut owners);
    owners
}

fn collect_source_column_owners(engine: &Engine, source: &SourcePlan, owners: &mut ColumnOwners) {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
        } => {
            let qualifier = alias.as_deref().unwrap_or(qualifier);
            let mut columns = engine.try_table_columns(name).unwrap_or_default();
            if columns.is_empty() {
                columns = engine
                    .view_definition(name)
                    .ok()
                    .flatten()
                    .as_ref()
                    .and_then(|view| {
                        view.output_columns
                            .clone()
                            .or_else(|| query_plan_output_columns(&view.query))
                    })
                    .unwrap_or_default();
            }
            if columns.is_empty() {
                columns = engine.foreign_table_columns(name).unwrap_or_default();
            }
            register_column_owners(owners, qualifier, columns);
        }
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if alias.is_none() {
                collect_source_column_owners(engine, left, owners);
                collect_source_column_owners(engine, right, owners);
            }
        }
        SourcePlan::Values {
            rows,
            alias: Some(alias),
            column_aliases,
        } => {
            let columns = if column_aliases.is_empty() {
                (1..=rows.first().map_or(0, Vec::len))
                    .map(|index| format!("column{index}"))
                    .collect()
            } else {
                column_aliases.clone()
            };
            register_column_owners(owners, alias, columns);
        }
        SourcePlan::Subquery {
            body,
            alias: Some(alias),
            column_aliases,
        } => {
            let columns = if column_aliases.is_empty() {
                query_plan_output_columns(body).unwrap_or_default()
            } else {
                column_aliases.clone()
            };
            register_column_owners(owners, alias, columns);
        }
        SourcePlan::Function {
            alias: Some(alias),
            column_aliases,
            ..
        } if !column_aliases.is_empty() => {
            register_column_owners(owners, alias, column_aliases.clone());
        }
        SourcePlan::Values { alias: None, .. }
        | SourcePlan::Function { .. }
        | SourcePlan::Subquery { alias: None, .. } => {}
    }
}

fn register_column_owners(
    owners: &mut ColumnOwners,
    qualifier: &str,
    columns: impl IntoIterator<Item = String>,
) {
    for column in columns {
        owners
            .entry(column)
            .and_modify(|owner| *owner = None)
            .or_insert_with(|| Some(qualifier.to_string()));
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

pub(in crate::sql) fn collect_cte_source_references(
    source: &SourcePlan,
    cte_names: &BTreeSet<&str>,
    references: &mut BTreeMap<String, Vec<String>>,
) {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
        } if cte_names.contains(name.as_str()) => {
            references
                .entry(name.clone())
                .or_default()
                .push(alias.clone().unwrap_or_else(|| qualifier.clone()));
        }
        SourcePlan::Join { left, right, .. } => {
            collect_cte_source_references(left, cte_names, references);
            collect_cte_source_references(right, cte_names, references);
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::Subquery { .. } => {}
    }
}

/// Specialize a physical query plan with a predicate on its output columns.
/// The caller keeps the original predicate as a residual check; this function
/// only returns a plan when pushing the predicate below the output boundary is
/// provably safe.
pub(in crate::sql) fn push_output_filter_into_query_plan(
    engine: &Engine,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Result<Option<QueryPlan>, SQLError> {
    if expr_contains_subquery(filter)
        || expr_contains_volatile_function(engine, filter)
        || query_contains_volatile_function(engine, plan)?
    {
        return Ok(None);
    }
    let Some(specialized) =
        specialize_query_output_filter(engine, plan, qualifier, filter, output_columns_override)
    else {
        return Ok(None);
    };
    match optimize_engine_plan(engine, UnifiedPlan::Query(Box::new(specialized)))? {
        UnifiedPlan::Query(plan) => Ok(Some(*plan)),
        UnifiedPlan::Command(_) => Err(SQLError::Internal(
            "query optimizer changed a query into a command plan".into(),
        )),
    }
}

pub(in crate::sql) fn specialize_query_output_filter(
    engine: &Engine,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<QueryPlan> {
    let mut specialized = plan.clone();
    specialize_relational_output_filter(
        engine,
        &mut specialized.root,
        qualifier,
        filter,
        output_columns_override,
    )?;
    Some(specialized)
}

pub(in crate::sql) fn specialize_relational_output_filter(
    engine: &Engine,
    root: &mut RelationalPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<()> {
    match root {
        RelationalPlan::QueryBlock(block) => specialize_query_block_output_filter(
            engine,
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
                engine,
                left,
                qualifier,
                filter,
                Some(&output_columns),
            )?;
            let specialized_right = specialize_query_output_filter(
                engine,
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

pub(in crate::sql) fn query_plan_output_columns(plan: &QueryPlan) -> Option<Vec<String>> {
    match &plan.root {
        RelationalPlan::QueryBlock(block) => Some(projection_columns(&block.projections)),
        RelationalPlan::SetOp { left, .. } => query_plan_output_columns(left),
        RelationalPlan::Values { rows, .. } => rows.first().map(|row| {
            (1..=row.len())
                .map(|index| format!("column{index}"))
                .collect()
        }),
    }
}

pub(in crate::sql) fn specialize_query_block_output_filter(
    engine: &Engine,
    block: &mut QueryBlockPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<()> {
    if block.limit.is_some()
        || block.offset.is_some()
        || matches!(block.compute, ComputePlan::Window)
        || !block.distinct_on.is_empty()
        || !block.grouping_sets.is_empty()
    {
        return None;
    }

    let output_columns = output_columns_override.map_or_else(
        || projection_columns(&block.projections),
        <[String]>::to_vec,
    );
    if output_columns.len() != block.projections.len() {
        return None;
    }
    let mut used = BTreeSet::new();
    let rewritten = rewrite_output_filter(
        filter,
        qualifier,
        &output_columns,
        &block.projections,
        &mut used,
    )?;
    if used.is_empty() {
        return None;
    }

    for index in &used {
        let expression = &block.projections[*index].expr;
        if matches!(expression, ScalarExpr::Star)
            || expression.contains_window()
            || expr_contains_subquery(expression)
            || expr_contains_volatile_function(engine, expression)
        {
            return None;
        }
        if matches!(block.compute, ComputePlan::Aggregate)
            && !block.group_by.iter().any(|group| group == expression)
        {
            return None;
        }
    }
    if block.distinct
        && block
            .projections
            .iter()
            .enumerate()
            .any(|(index, projection)| {
                !used.contains(&index) && expr_contains_function(&projection.expr)
            })
    {
        return None;
    }

    block.r#where = match block.r#where.take() {
        Some(existing) => Some(ScalarExpr::And(vec![existing, rewritten])),
        None => Some(rewritten),
    };
    Some(())
}

pub(in crate::sql) fn rewrite_output_filter(
    expression: &ScalarExpr,
    qualifier: &str,
    output_columns: &[String],
    projections: &[ProjectionPlan],
    used: &mut BTreeSet<usize>,
) -> Option<ScalarExpr> {
    let map_column = |column: &str, used: &mut BTreeSet<usize>| {
        let index = output_columns
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(column))?;
        used.insert(index);
        Some(projections[index].expr.clone())
    };
    let recur = |expression: &ScalarExpr, used: &mut BTreeSet<usize>| {
        rewrite_output_filter(expression, qualifier, output_columns, projections, used)
    };

    Some(match expression {
        ScalarExpr::Column(column) => map_column(column, used)?,
        ScalarExpr::QualifiedColumn {
            qualifier: expression_qualifier,
            column,
            ..
        } if expression_qualifier.eq_ignore_ascii_case(qualifier) => map_column(column, used)?,
        ScalarExpr::Default
        | ScalarExpr::Position(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => return None,
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => expression.clone(),
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
        ScalarExpr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
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
                    Some(uqa_execution::ScalarOrder {
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
        ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(recur(expr, used)?),
            ty: ty.clone(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::Value;
    use uqa_planner::{AccessPathPlan, JoinExecutionStrategy};
    use uqa_sql::ast::{BinaryOp, JoinKind};

    fn equality(left: &str, right: &str) -> ScalarExpr {
        ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(ScalarExpr::Column(left.into())),
            rhs: Box::new(ScalarExpr::Column(right.into())),
        }
    }

    fn qualified_literal_equality(qualifier: &str, column: &str, value: &str) -> ScalarExpr {
        ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(ScalarExpr::qualified_column(qualifier, column)),
            rhs: Box::new(ScalarExpr::Literal(Value::Str(value.into()))),
        }
    }

    fn joined_source(kind: JoinKind, on: ScalarExpr) -> SourcePlan {
        SourcePlan::Join {
            left: Box::new(SourcePlan::Table {
                name: "left_table".into(),
                qualifier: "left_table".into(),
                alias: Some("l".into()),
            }),
            right: Box::new(SourcePlan::Table {
                name: "right_table".into(),
                qualifier: "right_table".into(),
                alias: Some("r".into()),
            }),
            kind,
            on: Some(on),
            using: None,
            natural: false,
            alias: None,
            column_aliases: Vec::new(),
            lateral: false,
            strategy: JoinExecutionStrategy::Hash,
        }
    }

    fn query_block(filter: ScalarExpr, from: SourcePlan) -> QueryBlockPlan {
        QueryBlockPlan {
            projections: Vec::new(),
            from: Some(from),
            r#where: Some(filter),
            compute: ComputePlan::Project,
            group_by: Vec::new(),
            grouping_sets: Vec::new(),
            group_distinct: false,
            having: None,
            order_by: Vec::new(),
            limit: None,
            with_ties: false,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
            subqueries: Vec::new(),
            access: AccessPathPlan::Row,
            locking: Vec::new(),
        }
    }

    #[test]
    fn unique_unqualified_owner_enables_safe_filter_pushdown() {
        let engine = Engine::new();
        let owners = BTreeMap::from([
            ("p_name".into(), Some("part".into())),
            ("shared".into(), None),
        ]);
        let qualifiers = BTreeSet::from(["part".into(), "lineitem".into()]);
        let predicate = ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(ScalarExpr::Column("p_name".into())),
            rhs: Box::new(ScalarExpr::Literal(Value::Str("green".into()))),
        };

        let (qualifier, pushed) =
            qualifier_filter_for_part(&engine, &predicate, &qualifiers, None, &owners, &[])
                .unwrap();
        assert_eq!(qualifier, "part");
        let ScalarExpr::Binary { lhs, .. } = pushed else {
            panic!("pushdown changed the predicate shape");
        };
        assert!(matches!(
            lhs.as_ref(),
            ScalarExpr::QualifiedColumn { qualifier, column, .. }
                if qualifier == "part" && column == "p_name"
        ));

        let ambiguous = ScalarExpr::Column("shared".into());
        assert!(
            qualifier_filter_for_part(&engine, &ambiguous, &qualifiers, None, &owners, &[])
                .is_none()
        );
    }

    #[test]
    fn disjunction_derives_a_necessary_filter_for_every_complete_source_projection() {
        let engine = Engine::new();
        let qualifiers = BTreeSet::from(["n1".into(), "n2".into()]);
        let predicate = ScalarExpr::Or(vec![
            ScalarExpr::And(vec![
                qualified_literal_equality("n1", "name", "FRANCE"),
                qualified_literal_equality("n2", "name", "GERMANY"),
            ]),
            ScalarExpr::And(vec![
                qualified_literal_equality("n1", "name", "GERMANY"),
                qualified_literal_equality("n2", "name", "FRANCE"),
            ]),
        ]);

        let derived = derived_disjunctive_qualifier_filters(
            &engine,
            &predicate,
            &qualifiers,
            None,
            &BTreeMap::new(),
            &[],
        );
        assert_eq!(derived.len(), 2);
        for (qualifier, predicate) in derived {
            let ScalarExpr::Or(disjuncts) = predicate else {
                panic!("expected a projected disjunction")
            };
            assert_eq!(disjuncts.len(), 2);
            assert!(disjuncts
                .iter()
                .all(|part| expr_qualifiers(part) == BTreeSet::from([qualifier.clone()])));
        }
    }

    #[test]
    fn disjunction_does_not_push_a_projection_missing_from_any_branch() {
        let engine = Engine::new();
        let qualifiers = BTreeSet::from(["n1".into(), "n2".into()]);
        let predicate = ScalarExpr::Or(vec![
            qualified_literal_equality("n1", "name", "FRANCE"),
            qualified_literal_equality("n2", "name", "GERMANY"),
        ]);

        assert!(derived_disjunctive_qualifier_filters(
            &engine,
            &predicate,
            &qualifiers,
            None,
            &BTreeMap::new(),
            &[],
        )
        .is_empty());
    }

    #[test]
    fn inner_join_guarantee_elides_duplicate_where_conjunct() {
        let engine = Engine::new();
        let join_equality = equality("l.key", "r.key");
        let residual = ScalarExpr::Literal(Value::Bool(true));
        let from = joined_source(JoinKind::Inner, join_equality.clone());
        let block = query_block(
            ScalarExpr::And(vec![join_equality, residual.clone()]),
            from.clone(),
        );

        assert_eq!(
            final_filter_after_qualifier_pushdown(&engine, &block, &from, None),
            Some(residual)
        );
    }

    #[test]
    fn outer_join_keeps_duplicate_where_conjunct() {
        let engine = Engine::new();
        let join_equality = equality("l.key", "r.key");
        let filter = ScalarExpr::And(vec![
            join_equality.clone(),
            ScalarExpr::Literal(Value::Bool(true)),
        ]);
        let from = joined_source(JoinKind::Left, join_equality);
        let block = query_block(filter.clone(), from.clone());

        assert_eq!(
            final_filter_after_qualifier_pushdown(&engine, &block, &from, None),
            Some(filter)
        );
    }
}
