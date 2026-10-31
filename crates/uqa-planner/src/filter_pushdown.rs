//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Qualifier and CTE output-filter pushdown and specialization.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use uqa_sql::plan::UnifiedPlan;
use uqa_sql::semantics::{expr_contains_subquery, expr_qualifiers, flatten_and_filter_parts};
use uqa_sql::{
    plan::{
        source_projection::QualifierFilters, ProjectionPlan, QueryBlockPlan, QueryPlan,
        RelationalPlan, SourcePlan,
    },
    SQLError, ScalarExpr,
};
pub mod context;
use context::{FilterPushdownContext, FilterPushdownScope};
mod output_boundary;
mod specialization;
pub use specialization::{
    push_derived_output_filter_with_control, push_output_filter_into_query_plan_with_control,
    specialize_query_output_filter_with_control,
};
pub(crate) mod owned;
mod source_columns;
#[cfg(test)]
use uqa_sql::semantics::{from_qualifier_set, qualify_unqualified_columns};

#[cfg(test)]
use source_columns::SourceColumns;

mod cte_filters;
mod placement;
pub use cte_filters::cte_output_filters_with_control;
pub use placement::{
    final_filter_after_qualifier_pushdown_with_control, qualifier_filters_for_stmt_with_control,
};
pub use uqa_sql::semantics::outer_join_nullable_qualifiers;

pub fn qualifier_filters_for_stmt(
    context: FilterPushdownContext<'_>,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    scope: FilterPushdownScope<'_>,
) -> Result<Option<QualifierFilters>, SQLError> {
    qualifier_filters_for_stmt_with_control(
        context,
        stmt,
        from,
        scope,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}
pub fn final_filter_after_qualifier_pushdown(
    context: FilterPushdownContext<'_>,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filters: Option<&QualifierFilters>,
    scope: FilterPushdownScope<'_>,
) -> Result<Option<ScalarExpr>, SQLError> {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    final_filter_after_qualifier_pushdown_with_control(
        context, stmt, from, filters, scope, &control,
    )?
    .map(|filter| {
        Ok(filter
            .finish(&control)?
            .into_uncontrolled()
            .expect("ordinary residual predicate"))
    })
    .transpose()
}
#[cfg(test)]
fn qualifier_filter_for_part(
    context: FilterPushdownContext<'_>,
    part: &ScalarExpr,
    _: &BTreeSet<String>,
    columns: &SourceColumns,
    subqueries: &[QueryPlan],
) -> Option<(String, ScalarExpr)> {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let qualifier =
        placement::qualifier_for_part(context, part, columns, subqueries, &control).unwrap()?;
    Some((
        qualifier.to_owned(),
        qualify_unqualified_columns(part, qualifier),
    ))
}
#[cfg(test)]
fn derived_disjunctive_qualifier_filters(
    context: FilterPushdownContext<'_>,
    part: &ScalarExpr,
    _: &BTreeSet<String>,
    columns: &SourceColumns,
    subqueries: &[QueryPlan],
) -> Vec<(String, ScalarExpr)> {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let mut output = Vec::new();
    placement::derived_disjuncts(
        context,
        part,
        columns,
        subqueries,
        &control,
        &mut |qualifier, expression| {
            output.push((
                qualifier.to_owned(),
                expression
                    .finish(&control)?
                    .into_uncontrolled()
                    .expect("ordinary derived predicate"),
            ));
            Ok(())
        },
    )
    .unwrap();
    output
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
) -> Result<uqa_sql::plan::source_projection::CteOutputFilters, SQLError> {
    cte_output_filters_with_control(
        context,
        plan,
        scope,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

mod subqueries;
pub use subqueries::collect_subquery_ids;

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
    columns: Option<&[String]>,
) -> Result<Option<uqa_sql::plan::OwnedQueryPlan>, SQLError> {
    push_output_filter_into_query_plan_with_control(
        context,
        plan,
        qualifier,
        filter,
        columns,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

/// Place an ordinary source predicate before its outputs. Unobserved volatile outputs do not prevent a pure predicate on other columns from selecting their input rows.
pub fn push_derived_output_filter(
    context: FilterPushdownContext<'_>,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    columns: Option<&[String]>,
) -> Result<Option<uqa_sql::plan::OwnedQueryPlan>, SQLError> {
    push_derived_output_filter_with_control(
        context,
        plan,
        qualifier,
        filter,
        columns,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub fn specialize_query_output_filter(
    context: FilterPushdownContext<'_>,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<QueryPlan> {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    specialize_query_output_filter_with_control(
        context,
        plan,
        qualifier,
        filter,
        output_columns_override,
        &control,
    )
    .ok()
    .flatten()
    .map(|query| {
        query
            .finish(&control)
            .expect("ordinary specialized query")
            .into_uncontrolled()
            .expect("ordinary query ownership")
    })
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
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    owned::derived_output_predicate(
        owned::PredicateServices {
            routines,
            volatility,
        },
        block,
        qualifier,
        filter,
        output_columns_override,
        retain_query_slots,
        &control,
    )
    .unwrap_or(None)
    .map(|expression| {
        expression
            .finish(&control)
            .expect("ordinary scalar completion")
            .into_uncontrolled()
            .expect("ordinary scalar ownership")
    })
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

fn rewrite_output_filter_inputs(
    expression: &ScalarExpr,
    qualifier: &str,
    output_columns: &[String],
    projections: &[ProjectionPlan],
    used: &mut BTreeSet<usize>,
    retain_query_slots: bool,
) -> Option<ScalarExpr> {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    owned::rewrite_output_filter(
        expression,
        qualifier,
        output_columns,
        projections,
        &mut |index| {
            used.insert(index);
            Ok(())
        },
        retain_query_slots,
        &control,
    )
    .expect("ordinary output-reference substitution")
    .map(|expression| {
        expression
            .finish(&control)
            .expect("ordinary scalar completion")
            .into_uncontrolled()
            .expect("ordinary scalar ownership")
    })
}

#[cfg(test)]
mod tests;
