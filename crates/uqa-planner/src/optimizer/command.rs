//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command-owned query and scalar child traversal.

use super::{
    optimize_assignments, optimize_projections, optimize_query, optimize_scalar_slot,
    optimize_source, optimize_unified_plan, prioritize_access_predicates, AggregateClassifier,
    CommandPlan, ConflictActionPlan, ExpressionPlan, MergeWhenPlan, OptimizerConfig,
};

pub(super) fn optimize_command(
    command: &mut CommandPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) {
    match command {
        CommandPlan::Insert(plan) => {
            for cte in &mut plan.ctes {
                optimize_query(&mut cte.query, config, aggregates);
            }
            if let Some(source) = &mut plan.source {
                optimize_query(source, config, aggregates);
            }
            for row in &mut plan.rows {
                for expression in row {
                    optimize_scalar_slot(expression, config);
                }
            }
            if let Some(conflict) = &mut plan.on_conflict {
                if let ConflictActionPlan::Update {
                    assignments,
                    predicate,
                } = &mut conflict.action
                {
                    optimize_assignments(assignments, config);
                    if let Some(predicate) = predicate {
                        optimize_scalar_slot(predicate, config);
                    }
                }
            }
            optimize_projections(&mut plan.returning, config);
            for subquery in &mut plan.subqueries {
                optimize_query(subquery, config, aggregates);
            }
        }
        CommandPlan::Update(plan) => {
            for cte in &mut plan.ctes {
                optimize_query(&mut cte.query, config, aggregates);
            }
            if let Some(source) = &mut plan.source {
                optimize_source(source, config, aggregates);
            }
            optimize_assignments(&mut plan.assignments, config);
            if let Some(predicate) = &mut plan.predicate {
                optimize_scalar_slot(predicate, config);
                if config.enable_filter_pushdown {
                    prioritize_access_predicates(predicate);
                }
            }
            optimize_projections(&mut plan.returning, config);
            for subquery in &mut plan.subqueries {
                optimize_query(subquery, config, aggregates);
            }
        }
        CommandPlan::Delete(plan) => {
            for cte in &mut plan.ctes {
                optimize_query(&mut cte.query, config, aggregates);
            }
            if let Some(source) = &mut plan.source {
                optimize_source(source, config, aggregates);
            }
            if let Some(predicate) = &mut plan.predicate {
                optimize_scalar_slot(predicate, config);
                if config.enable_filter_pushdown {
                    prioritize_access_predicates(predicate);
                }
            }
            optimize_projections(&mut plan.returning, config);
            for subquery in &mut plan.subqueries {
                optimize_query(subquery, config, aggregates);
            }
        }
        CommandPlan::Merge(plan) => {
            optimize_source(&mut plan.source, config, aggregates);
            optimize_scalar_slot(&mut plan.join_condition, config);
            for clause in &mut plan.when_clauses {
                match clause {
                    MergeWhenPlan::UpdateMatched {
                        condition,
                        assignments,
                    }
                    | MergeWhenPlan::UpdateNotMatchedBySource {
                        condition,
                        assignments,
                    } => {
                        if let Some(condition) = condition {
                            optimize_scalar_slot(condition, config);
                        }
                        optimize_assignments(assignments, config);
                    }
                    MergeWhenPlan::DeleteMatched { condition }
                    | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                    | MergeWhenPlan::NothingMatched { condition }
                    | MergeWhenPlan::NothingNotMatched { condition }
                    | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                        if let Some(condition) = condition {
                            optimize_scalar_slot(condition, config);
                        }
                    }
                    MergeWhenPlan::InsertNotMatched {
                        condition, values, ..
                    } => {
                        if let Some(condition) = condition {
                            optimize_scalar_slot(condition, config);
                        }
                        for value in values {
                            optimize_scalar_slot(value, config);
                        }
                    }
                }
            }
            optimize_projections(&mut plan.returning, config);
            for subquery in &mut plan.subqueries {
                optimize_query(subquery, config, aggregates);
            }
        }
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => {
            optimize_query(query, config, aggregates);
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            optimize_unified_plan(body, config, aggregates);
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for expression in params {
                optimize_expression_plan(expression, config, aggregates);
            }
        }
        CommandPlan::CreateTable(_)
        | CommandPlan::CreateIndex(_)
        | CommandPlan::Drop(_)
        | CommandPlan::AlterTable(_)
        | CommandPlan::AlterViewOptions(_)
        | CommandPlan::RefreshMaterializedView { .. }
        | CommandPlan::CreateSchema { .. }
        | CommandPlan::SetVariable { .. }
        | CommandPlan::ResetVariable { .. }
        | CommandPlan::ResetAllVariables
        | CommandPlan::SetConstraints { .. }
        | CommandPlan::ShowVariable { .. }
        | CommandPlan::Discard { .. }
        | CommandPlan::Load { .. }
        | CommandPlan::Analyze { .. }
        | CommandPlan::Vacuum(_)
        | CommandPlan::Truncate { .. }
        | CommandPlan::Transaction(_)
        | CommandPlan::FetchCursor(_)
        | CommandPlan::CloseCursor { .. }
        | CommandPlan::CreateSequence(_)
        | CommandPlan::AlterSequence(_)
        | CommandPlan::Deallocate { .. }
        | CommandPlan::CreateForeignServer(_)
        | CommandPlan::CreateForeignTable(_)
        | CommandPlan::CreateFunction(_)
        | CommandPlan::DropFunction(_)
        | CommandPlan::AlterRoutine(_)
        | CommandPlan::AlterRoutineOwner(_)
        | CommandPlan::GrantRoutine(_)
        | CommandPlan::CreateRole(_)
        | CommandPlan::AlterRole(_)
        | CommandPlan::DropRole(_)
        | CommandPlan::CreateTrigger(_)
        | CommandPlan::DropTrigger(_)
        | CommandPlan::CreateRule(_)
        | CommandPlan::DropRule(_)
        | CommandPlan::DoBlock { .. } => {}
    }
}

fn optimize_expression_plan(
    expression: &mut ExpressionPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) {
    optimize_scalar_slot(&mut expression.scalar, config);
    for subquery in &mut expression.subqueries {
        optimize_query(subquery, config, aggregates);
    }
}
