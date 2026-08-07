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
    let mut filters = QualifierFilters::new();
    for part in flatten_and_filter_parts(filter) {
        if let Some((qualifier, filter)) =
            qualifier_filter_for_part(engine, part, &from_quals, single_qualifier.as_deref())
        {
            filters.entry(qualifier).or_default().push(filter);
        }
    }
    (!filters.is_empty()).then_some(filters)
}

pub(in crate::sql) fn qualifier_filter_for_part(
    engine: &Engine,
    part: &ScalarExpr,
    from_quals: &BTreeSet<String>,
    single_qualifier: Option<&str>,
) -> Option<(String, ScalarExpr)> {
    if expr_contains_subquery(part)
        || (expr_contains_volatile_function(engine, part)
            && !uqa_planner::optimizer::contains_retrieval(part))
    {
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
    if filters.is_none() || !qualifier_filter_elision_safe(from) {
        return Some(filter.clone());
    }
    let from_quals = from_qualifier_set(from);
    let single_qualifier = (from_quals.len() == 1)
        .then(|| from_quals.iter().next().cloned())
        .flatten();
    let residual: Vec<ScalarExpr> = flatten_and_filter_parts(filter)
        .into_iter()
        .filter(|part| {
            qualifier_filter_for_part(engine, part, &from_quals, single_qualifier.as_deref())
                .is_none()
        })
        .cloned()
        .collect();
    combine_filter_parts(residual)
}

pub(in crate::sql) fn qualifier_filter_elision_safe(from: &SourcePlan) -> bool {
    match from {
        SourcePlan::Join {
            left, right, kind, ..
        } => {
            matches!(
                kind,
                uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross
            ) && qualifier_filter_elision_safe(left)
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
    let mut grouped: BTreeMap<String, (String, Vec<ScalarExpr>)> = BTreeMap::new();
    for part in flatten_and_filter_parts(filter) {
        let Some((qualifier, predicate)) =
            qualifier_filter_for_part(engine, part, &from_qualifiers, single_qualifier.as_deref())
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

    grouped
        .into_iter()
        .filter_map(|(name, (qualifier, predicates))| {
            combine_filter_parts(predicates).map(|predicate| (name, (qualifier, predicate)))
        })
        .collect()
}

pub(in crate::sql) fn collect_cte_source_references(
    source: &SourcePlan,
    cte_names: &BTreeSet<&str>,
    references: &mut BTreeMap<String, Vec<String>>,
) {
    match source {
        SourcePlan::Table { name, alias } if cte_names.contains(name.as_str()) => {
            references
                .entry(name.clone())
                .or_default()
                .push(alias.clone().unwrap_or_else(|| name.clone()));
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
        ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Star
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
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
            name: name.clone(),
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
