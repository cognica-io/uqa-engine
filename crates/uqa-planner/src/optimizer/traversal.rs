//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive query/source plan traversal and compute classification.

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
) {
    match plan {
        UnifiedPlan::Query(query) => optimize_query(query, config, aggregates),
        UnifiedPlan::Command(command) => optimize_command(command, config, aggregates),
    }
}

pub(super) fn optimize_query(
    query: &mut QueryPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) {
    for cte in &mut query.ctes {
        optimize_query(&mut cte.query, config, aggregates);
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => optimize_query_block(block, config, aggregates),
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            optimize_query(left, config, aggregates);
            optimize_query(right, config, aggregates);
            for order in order_by {
                optimize_scalar_slot(&mut order.expr, config);
            }
            if let Some(limit) = limit {
                optimize_scalar_slot(limit, config);
            }
            if let Some(offset) = offset {
                optimize_scalar_slot(offset, config);
            }
            for subquery in subqueries {
                optimize_query(subquery, config, aggregates);
            }
        }
        RelationalPlan::Values { rows, subqueries } => {
            for row in rows {
                for expression in row {
                    optimize_scalar_slot(expression, config);
                }
            }
            for subquery in subqueries {
                optimize_query(subquery, config, aggregates);
            }
        }
    }
}

pub(super) fn optimize_query_block(
    block: &mut QueryBlockPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) {
    if let Some(source) = &mut block.from {
        optimize_source(source, config, aggregates);
    }
    for subquery in &mut block.subqueries {
        optimize_query(subquery, config, aggregates);
    }
    for projection in &mut block.projections {
        optimize_scalar_slot(&mut projection.expr, config);
    }
    if let Some(predicate) = &mut block.r#where {
        optimize_scalar_slot(predicate, config);
        let allow_unqualified_signals = source_allows_unqualified_signals(block.from.as_ref());
        rewrite_implicit_hybrid_fusion(predicate, allow_unqualified_signals);
        if config.enable_filter_pushdown {
            prioritize_access_predicates(predicate);
        }
    }
    for expression in &mut block.group_by {
        optimize_scalar_slot(expression, config);
    }
    for set in &mut block.grouping_sets {
        for expression in set {
            optimize_scalar_slot(expression, config);
        }
    }
    if let Some(having) = &mut block.having {
        optimize_scalar_slot(having, config);
    }
    for order in &mut block.order_by {
        optimize_scalar_slot(&mut order.expr, config);
    }
    if let Some(limit) = &mut block.limit {
        optimize_scalar_slot(limit, config);
    }
    if let Some(offset) = &mut block.offset {
        optimize_scalar_slot(offset, config);
    }
    for expression in &mut block.distinct_on {
        optimize_scalar_slot(expression, config);
    }

    let is_aggregate = |name: &str| {
        crate::unified_plan::is_builtin_aggregate(name) || aggregates.is_registered_aggregate(name)
    };
    let has_aggregate = !block.group_by.is_empty()
        || !block.grouping_sets.is_empty()
        || block.having.is_some()
        || block
            .projections
            .iter()
            .any(|projection| projection.expr.contains_aggregate(&is_aggregate));
    let has_window = block
        .projections
        .iter()
        .any(|projection| projection.expr.contains_window());
    block.compute = if has_aggregate {
        ComputePlan::Aggregate
    } else if has_window {
        ComputePlan::Window
    } else {
        ComputePlan::Project
    };
    block.access = choose_access_path(block);
}

pub(super) fn optimize_source(
    source: &mut SourcePlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) {
    match source {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            optimize_source(left, config, aggregates);
            optimize_source(right, config, aggregates);
            if let Some(on) = on {
                optimize_scalar_slot(on, config);
                if config.enable_filter_pushdown {
                    prioritize_access_predicates(on);
                }
            }
        }
        SourcePlan::Values { rows, .. } => {
            for row in rows {
                for expression in row {
                    optimize_scalar_slot(expression, config);
                }
            }
        }
        SourcePlan::Function { args, .. } => {
            for expression in args {
                optimize_scalar_slot(expression, config);
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                for expression in &mut function.args {
                    optimize_scalar_slot(expression, config);
                }
            }
        }
        SourcePlan::Subquery { body, .. } => optimize_query(body, config, aggregates),
        SourcePlan::Table { .. } => {}
    }
}
