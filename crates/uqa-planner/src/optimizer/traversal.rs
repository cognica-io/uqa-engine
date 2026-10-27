//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive query/source plan traversal and compute classification.

use uqa_sql::{SQLError, ScalarExpr};

use super::{
    choose_access_path, optimize_command, optimize_scalar_slot, prioritize_access_predicates,
    rewrite_implicit_hybrid_fusion, source_allows_unqualified_signals, AggregateClassifier,
    ComputePlan, OptimizerConfig, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan,
    UnifiedPlan,
};

pub(super) fn optimize_unified_plan(
    plan: &mut UnifiedPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) -> Result<(), SQLError> {
    plan.visit_queries_mut(&mut |query| {
        if query.analyzed_output.is_none() {
            if let RelationalPlan::QueryBlock(block) = &mut query.root {
                block.compute = block.compute_kind(aggregates);
            }
        }
    });
    super::source_demand::prune(plan, config, aggregates)?;
    super::derived_pullup::rewrite(plan, config)?;
    // Analyzed layouts describe the original topology, not later source expansion or join order.
    plan.visit_queries_mut(&mut |query| {
        query.analyzed_source = None;
        // Discarded scalar arenas must not keep an enclosing command's CTE alive during constant evaluation.
        super::subqueries::prune_query(query);
    });
    match plan {
        UnifiedPlan::Query(query) => optimize_query(query, config, aggregates)?,
        UnifiedPlan::Command(command) => optimize_command(command, config, aggregates)?,
    }
    Ok(())
}

/// VALUES keeps the same bounded constant owner whether it is a command input, query root or range item.
pub(super) fn optimize_values(
    rows: &mut [Vec<uqa_sql::ScalarExpr>],
    config: &OptimizerConfig,
) -> Result<(), SQLError> {
    if rows.iter().any(|row| !row.is_empty()) {
        let values_config = config.for_values()?;
        for expression in rows.iter_mut().flatten() {
            optimize_scalar_slot(expression, &values_config)?;
        }
    }
    Ok(())
}

pub(super) fn optimize_cte(
    body: &mut crate::CtePlanBody,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) -> Result<(), SQLError> {
    match body {
        crate::CtePlanBody::Query(query) => optimize_query(query, config, aggregates)?,
        crate::CtePlanBody::Command(command) => optimize_command(command, config, aggregates)?,
    }
    Ok(())
}

pub(super) fn optimize_query(
    query: &mut QueryPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) -> Result<(), SQLError> {
    if let Some(grouping) = query.analyzed_grouping.take() {
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            return Err(SQLError::Internal(
                "analyzed grouping belongs to a query block".into(),
            ));
        };
        grouping.apply(block)?;
    }
    super::subqueries::prune_query(query);
    if query.analyzed_output.is_none() {
        if let Some(routines) = &config.routine_inlining {
            super::cte_projection::prune(
                query,
                aggregates,
                routines.volatility,
                Some(routines.routines),
            )?;
        }
    }
    let reachable = uqa_sql::semantics::reachable_plan_cte_names(query);
    for cte in query
        .ctes
        .iter_mut()
        .filter(|cte| reachable.contains(&cte.name))
    {
        optimize_cte(&mut cte.body, config, aggregates)?;
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            optimize_query_block(block, config, aggregates, query.analyzed_output.is_none())?;
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            optimize_query(left, config, aggregates)?;
            optimize_query(right, config, aggregates)?;
            for order in &mut *order_by {
                optimize_scalar_slot(&mut order.expr, config)?;
            }
            if let Some(limit) = limit {
                optimize_scalar_slot(limit, config)?;
            }
            if let Some(offset) = offset {
                optimize_scalar_slot(offset, config)?;
            }
            let live = super::subqueries::live_slots(
                order_by
                    .iter()
                    .map(|order| &order.expr)
                    .chain(limit.as_deref())
                    .chain(offset.as_deref()),
            )?;
            super::subqueries::optimize_live(subqueries, &live, config, aggregates)?;
        }
        RelationalPlan::Values { rows, subqueries } => {
            optimize_values(rows, config)?;
            let live = super::subqueries::live_slots(rows.iter().flatten())?;
            super::subqueries::optimize_live(subqueries, &live, config, aggregates)?;
        }
    }
    Ok(())
}

pub(super) fn optimize_query_block(
    block: &mut QueryBlockPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
    infer_output_demand: bool,
) -> Result<(), SQLError> {
    if infer_output_demand {
        let routines = config.routine_inlining.as_ref();
        super::derived_projection::prune(
            block,
            super::OutputPruningContext {
                aggregates,
                routines: routines.map(|context| context.routines),
                volatility: routines.map(|context| context.volatility),
                visible_ctes: &|_| false,
            },
        )?;
    }
    if let Some(source) = &mut block.from {
        optimize_source(source, config, aggregates)?;
    }
    super::source_constants::propagate_source_constants(
        block,
        config
            .routine_inlining
            .as_ref()
            .map(|context| context.types),
        config
            .routine_inlining
            .as_ref()
            .map(|context| context.volatility),
    )?;
    super::optimize_projections(&mut block.projections, config)?;
    if let Some(predicate) = &mut block.r#where {
        super::optimize_qualification(predicate, config)?;
        let allow_unqualified_signals = source_allows_unqualified_signals(block.from.as_ref());
        rewrite_implicit_hybrid_fusion(predicate, allow_unqualified_signals);
        if config.enable_filter_pushdown {
            prioritize_access_predicates(predicate);
        }
    }
    for expression in &mut block.group_by {
        optimize_scalar_slot(expression, config)?;
    }
    for set in &mut block.grouping_sets {
        for expression in set {
            optimize_scalar_slot(expression, config)?;
        }
    }
    if let Some(having) = &mut block.having {
        super::optimize_qualification(having, config)?;
    }
    for order in &mut block.order_by {
        optimize_scalar_slot(&mut order.expr, config)?;
    }
    if let Some(limit) = &mut block.limit {
        optimize_scalar_slot(limit, config)?;
    }
    if let Some(offset) = &mut block.offset {
        optimize_scalar_slot(offset, config)?;
    }
    for expression in &mut block.distinct_on {
        optimize_scalar_slot(expression, config)?;
    }
    for window in &mut block.windows {
        for expression in window.spec.expressions_mut() {
            optimize_scalar_slot(expression, config)?;
        }
    }

    super::source_constants::simplify_constant_group(
        block,
        config
            .routine_inlining
            .as_ref()
            .map(|context| context.types),
    );
    let live_subqueries = super::subqueries::live_slots_for_block(block)?;
    super::subqueries::optimize_live(&mut block.subqueries, &live_subqueries, config, aggregates)?;

    // Removing an unused aggregate's value does not remove its implicit group on empty input.
    if !matches!(block.compute, ComputePlan::Aggregate) {
        block.compute = block.compute_kind(aggregates);
    }
    block.access = choose_access_path(block);
    Ok(())
}

pub(super) fn optimize_source(
    source: &mut SourcePlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::RetainedRows(_) => {}
        SourcePlan::Join {
            left,
            right,
            on,
            lateral,
            ..
        } => {
            // Function FROM items are implicitly lateral. Keep that scope when SQL expansion replaces the function with a derived table.
            let function_lateral = matches!(
                right.as_ref(),
                SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
            );
            optimize_source(left, config, aggregates)?;
            optimize_source(right, config, aggregates)?;
            *lateral |= function_lateral && matches!(right.as_ref(), SourcePlan::Subquery { .. });
            if let Some(on) = on {
                super::optimize_qualification(on, config)?;
                if config.enable_filter_pushdown {
                    prioritize_access_predicates(on);
                }
            }
        }
        SourcePlan::Values { rows, .. } => {
            optimize_values(rows, config)?;
        }
        SourcePlan::Function { args, .. } => {
            for expression in args {
                optimize_scalar_slot(expression, config)?;
            }
            super::function_source::optimize(source, config, aggregates)?;
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                for expression in &mut function.args {
                    optimize_scalar_slot(expression, config)?;
                }
            }
            super::function_source::optimize_group(source, config, aggregates)?;
        }
        SourcePlan::Subquery { body, .. } => optimize_query(body, config, aggregates)?,
        SourcePlan::Table { .. } => {}
    }
    Ok(())
}
