//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command-owned query and scalar child traversal.

use uqa_sql::SQLError;

use super::{
    optimize_assignments, optimize_projections, optimize_query, optimize_scalar_slot,
    optimize_source, prioritize_access_predicates, AggregateClassifier, CommandPlan,
    ConflictActionPlan, ExpressionPlan, MergeWhenPlan, OptimizerConfig,
};

#[expect(
    clippy::too_many_lines,
    reason = "optimizer rewrite preserves exhaustive variants and fixed-point order"
)]
pub(super) fn optimize_command(
    command: &mut CommandPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) -> Result<(), SQLError> {
    super::subqueries::prune_command(command, config.production)?;
    let reachable = uqa_sql::semantics::reachable_command_cte_names(command);
    if let Some(ctes) = command.ctes_mut() {
        for cte in ctes.iter_mut().filter(|cte| reachable.contains(&cte.name)) {
            super::traversal::optimize_cte(&mut cte.body, config, aggregates)?;
        }
    }
    match command {
        CommandPlan::Insert(plan) => {
            if let Some(source) = &mut plan.source {
                optimize_query(source, config, aggregates)?;
            }
            super::traversal::optimize_values(&mut plan.rows, config)?;
            if let Some(conflict) = &mut plan.on_conflict {
                if let ConflictActionPlan::Update {
                    assignments,
                    predicate,
                } = &mut conflict.action
                {
                    optimize_assignments(assignments, config)?;
                    if let Some(predicate) = predicate {
                        super::optimize_qualification(predicate, config)?;
                    }
                }
            }
            optimize_projections(&mut plan.returning, config)?;
            let expressions = plan.expressions();
            let live = super::subqueries::live_slots(expressions, config.production)?;
            super::subqueries::optimize_live(&mut plan.subqueries, &live, config, aggregates)?;
        }
        CommandPlan::Update(plan) => {
            if let Some(source) = &mut plan.source {
                optimize_source(source, config, aggregates)?;
            }
            optimize_assignments(&mut plan.assignments, config)?;
            if let Some(predicate) = &mut plan.predicate {
                super::optimize_qualification(predicate, config)?;
                if config.enable_filter_pushdown {
                    prioritize_access_predicates(predicate);
                }
            }
            optimize_projections(&mut plan.returning, config)?;
            let live = super::subqueries::live_slots_with_source(
                plan.expressions(),
                plan.source.as_deref(),
                config.production,
            )?;
            super::subqueries::optimize_live(&mut plan.subqueries, &live, config, aggregates)?;
        }
        CommandPlan::Delete(plan) => {
            if let Some(source) = &mut plan.source {
                optimize_source(source, config, aggregates)?;
            }
            if let Some(predicate) = &mut plan.predicate {
                super::optimize_qualification(predicate, config)?;
                if config.enable_filter_pushdown {
                    prioritize_access_predicates(predicate);
                }
            }
            optimize_projections(&mut plan.returning, config)?;
            let live = super::subqueries::live_slots_with_source(
                plan.expressions(),
                plan.source.as_deref(),
                config.production,
            )?;
            super::subqueries::optimize_live(&mut plan.subqueries, &live, config, aggregates)?;
        }
        CommandPlan::Merge(plan) => {
            optimize_source(&mut plan.source, config, aggregates)?;
            super::optimize_qualification(&mut plan.join_condition, config)?;
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
                            super::optimize_qualification(condition, config)?;
                        }
                        optimize_assignments(assignments, config)?;
                    }
                    MergeWhenPlan::DeleteMatched { condition }
                    | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                    | MergeWhenPlan::NothingMatched { condition }
                    | MergeWhenPlan::NothingNotMatched { condition }
                    | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                        if let Some(condition) = condition {
                            super::optimize_qualification(condition, config)?;
                        }
                    }
                    MergeWhenPlan::InsertNotMatched {
                        condition, values, ..
                    } => {
                        if let Some(condition) = condition {
                            super::optimize_qualification(condition, config)?;
                        }
                        for value in values {
                            optimize_scalar_slot(value, config)?;
                        }
                    }
                }
            }
            optimize_projections(&mut plan.returning, config)?;
            let live = super::subqueries::live_slots_with_source(
                plan.expressions(),
                Some(&plan.source),
                config.production,
            )?;
            super::subqueries::optimize_live(&mut plan.subqueries, &live, config, aggregates)?;
        }
        CommandPlan::DeclareCursor { query, .. } => {
            optimize_query(query, config, aggregates)?;
        }
        CommandPlan::Call { args: params, .. } => {
            for expression in params {
                optimize_expression_plan(expression, config, aggregates)?;
            }
        }
        CommandPlan::Explain { .. }
        | CommandPlan::CreateTableAs { .. }
        | CommandPlan::CreateMaterializedView { .. }
        | CommandPlan::CreateView { .. }
        | CommandPlan::Execute { .. }
        | CommandPlan::Prepare { .. }
        | CommandPlan::CreateTable(_)
        | CommandPlan::CreateTableIfNotExists(_)
        | CommandPlan::CreateIndex(_)
        | CommandPlan::RenameIndex(_)
        | CommandPlan::Drop(_)
        | CommandPlan::AlterTable(_)
        | CommandPlan::AlterForeignTable(_)
        | CommandPlan::AlterView(_)
        | CommandPlan::RefreshMaterializedView { .. }
        | CommandPlan::CreateSchema { .. }
        | CommandPlan::AlterSchemaOwner { .. }
        | CommandPlan::RenameSchema { .. }
        | CommandPlan::Notify { .. }
        | CommandPlan::Listen { .. }
        | CommandPlan::Unlisten { .. }
        | CommandPlan::SetVariable { .. }
        | CommandPlan::ResetVariable { .. }
        | CommandPlan::ResetAllVariables
        | CommandPlan::SetConstraints { .. }
        | CommandPlan::ShowVariable { .. }
        | CommandPlan::Discard { .. }
        | CommandPlan::Load { .. }
        | CommandPlan::Analyze { .. }
        | CommandPlan::LockTable(_)
        | CommandPlan::Vacuum(_)
        | CommandPlan::Truncate { .. }
        | CommandPlan::Transaction(_)
        | CommandPlan::FetchCursor(_)
        | CommandPlan::CloseCursor { .. }
        | CommandPlan::CreateSequence(_)
        | CommandPlan::CreateDomain(_)
        | CommandPlan::AlterDomain(_)
        | CommandPlan::CommentType(_)
        | CommandPlan::CreateShellType(_)
        | CommandPlan::CreateEnum(_)
        | CommandPlan::CreateCompositeType(_)
        | CommandPlan::AlterEnum(_)
        | CommandPlan::AlterTypeObject(_)
        | CommandPlan::GrantType(_)
        | CommandPlan::AlterSequence(_)
        | CommandPlan::Deallocate { .. }
        | CommandPlan::CreateForeignWrapper(_)
        | CommandPlan::CreateForeignServer(_)
        | CommandPlan::CreateForeignTable(_)
        | CommandPlan::CreateForeignTableDefinition(_)
        | CommandPlan::CreateFunction(_)
        | CommandPlan::DropFunction(_)
        | CommandPlan::AlterRoutine(_)
        | CommandPlan::AlterRoutineOwner(_)
        | CommandPlan::RenameRoutine(_)
        | CommandPlan::GrantRoutine(_)
        | CommandPlan::GrantTable(_)
        | CommandPlan::GrantSequence(_)
        | CommandPlan::GrantDatabase(_)
        | CommandPlan::GrantSchema(_)
        | CommandPlan::GrantRole(_)
        | CommandPlan::CreateRole(_)
        | CommandPlan::AlterRole(_)
        | CommandPlan::RenameRole(_)
        | CommandPlan::DropRole(_)
        | CommandPlan::CreateTrigger(_)
        | CommandPlan::DropTrigger(_)
        | CommandPlan::CreateRule(_)
        | CommandPlan::DropRule(_)
        | CommandPlan::DoBlock { .. }
        | CommandPlan::Empty => {}
    }
    Ok(())
}

fn optimize_expression_plan(
    expression: &mut ExpressionPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) -> Result<(), SQLError> {
    optimize_scalar_slot(&mut expression.scalar, config)?;
    let live = super::subqueries::live_slots([&expression.scalar], config.production)?;
    super::subqueries::optimize_live(&mut expression.subqueries, &live, config, aggregates)?;
    Ok(())
}
