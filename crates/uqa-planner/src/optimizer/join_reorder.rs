//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statistics-driven DPccp join reordering and predicate attachment.

use super::{
    BTreeMap, BTreeSet, BinaryOp, CommandPlan, ExpressionPlan, JoinExecutionStrategy,
    JoinGraphError, JoinGraphResult, JoinOrderOptimizer, JoinPredicate, QueryPlan, RelationalPlan,
    ScalarExpr, ScalarFrameBound, SourcePlan, SourceStatistics, UnifiedPlan,
};

use uqa_core::memory::{Produced, ProductionControl, ProductionSmallVec, ProductionVec};

use super::OptimizerResult;

const DEFAULT_JOIN_CARDINALITY: u64 = 1_000;

#[cfg(test)]
mod tests;

mod bindings;
mod owned;
mod selection;
pub(super) use owned::reorder_owned_plan;
mod inference;
mod inputs;
use bindings::{PredicateAttachments, PredicateBindings};
mod output;
use inference::{implied_join_predicates, join_predicate};
use output::Output;
mod materialization;
mod relations;
use inputs::{
    collect_conjuncts, flatten_reorderable_inner_join, local_predicate_groups, ColumnOwners,
};
use relations::RelationMetadata;

struct JoinRewrite {
    // Destroy the partially rewritten tree before its output leases on every failure path.
    plan: UnifiedPlan,
    output: Output,
}

impl JoinRewrite {
    fn new(plan: UnifiedPlan, control: &ProductionControl<'_>) -> Self {
        Self {
            plan,
            output: Output::new(control),
        }
    }

    fn run(
        &mut self,
        statistics: &dyn SourceStatistics,
        control: &ProductionControl<'_>,
    ) -> OptimizerResult<()> {
        match &mut self.plan {
            UnifiedPlan::Query(query) => {
                reorder_query_joins(query, statistics, control, &mut self.output)
            }
            UnifiedPlan::Command(command) => {
                reorder_command_joins(command, statistics, control, &mut self.output)
            }
        }
    }
}

pub(super) fn reorder_unified_plan_joins(
    plan: UnifiedPlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<uqa_sql::plan::OwnedExecutablePlan> {
    let mut rewrite = JoinRewrite::new(plan, control);
    rewrite.run(statistics, control)?;
    Ok(uqa_sql::plan::OwnedExecutablePlan::from_parts(
        rewrite.plan,
        rewrite.output.finish(),
    ))
}

fn reorder_query_joins(
    query: &mut QueryPlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
    output: &mut Output,
) -> OptimizerResult<()> {
    for cte in &mut query.ctes {
        match &mut cte.body {
            crate::CtePlanBody::Query(query) => {
                reorder_query_joins(query, statistics, control, output)?;
            }
            crate::CtePlanBody::Command(command) => {
                reorder_command_joins(command, statistics, control, output)?;
            }
        }
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                let mut where_predicates = ProductionVec::new(*control);
                if matches!(source, SourcePlan::Join { .. }) {
                    if let Some(predicate) = &block.r#where {
                        collect_conjuncts(predicate, &mut where_predicates, control)?;
                    }
                }
                reorder_source_joins(source, &where_predicates, statistics, control, output)?;
            }
            for subquery in &mut block.subqueries {
                reorder_query_joins(subquery, statistics, control, output)?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            reorder_query_joins(left, statistics, control, output)?;
            reorder_query_joins(right, statistics, control, output)?;
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                reorder_query_joins(subquery, statistics, control, output)?;
            }
        }
    }
    Ok(())
}

fn reorder_cte_joins(
    cte: &mut crate::CtePlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
    output: &mut Output,
) -> OptimizerResult<()> {
    match &mut cte.body {
        crate::CtePlanBody::Query(query) => reorder_query_joins(query, statistics, control, output),
        crate::CtePlanBody::Command(command) => {
            reorder_command_joins(command, statistics, control, output)
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "traverses each command kind and its owned children"
)]
fn reorder_command_joins(
    command: &mut CommandPlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
    output: &mut Output,
) -> OptimizerResult<()> {
    match command {
        CommandPlan::Insert(plan) => reorder_insert_joins(plan, statistics, control, output)?,
        CommandPlan::Update(plan) => {
            for cte in &mut plan.ctes {
                reorder_cte_joins(cte, statistics, control, output)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_source_joins(source, &[], statistics, control, output)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics, control, output)?;
            }
        }
        CommandPlan::Delete(plan) => {
            for cte in &mut plan.ctes {
                reorder_cte_joins(cte, statistics, control, output)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_source_joins(source, &[], statistics, control, output)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics, control, output)?;
            }
        }
        CommandPlan::Merge(plan) => {
            for cte in &mut plan.ctes {
                reorder_cte_joins(cte, statistics, control, output)?;
            }
            reorder_source_joins(&mut plan.source, &[], statistics, control, output)?;
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics, control, output)?;
            }
        }
        CommandPlan::DeclareCursor { query, .. } => {
            reorder_query_joins(query, statistics, control, output)?;
        }
        CommandPlan::Call { args: params, .. } => {
            reorder_expression_subquery_joins(params, statistics, control, output)?;
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
        | CommandPlan::RefreshMaterializedView { .. }
        | CommandPlan::AlterTable(_)
        | CommandPlan::AlterForeignTable(_)
        | CommandPlan::AlterView(_)
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

fn reorder_insert_joins(
    plan: &mut crate::unified_plan::InsertPlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
    output: &mut Output,
) -> OptimizerResult<()> {
    for cte in &mut plan.ctes {
        match &mut cte.body {
            crate::CtePlanBody::Query(query) => {
                reorder_query_joins(query, statistics, control, output)?;
            }
            crate::CtePlanBody::Command(command) => {
                reorder_command_joins(command, statistics, control, output)?;
            }
        }
    }
    if let Some(source) = &mut plan.source {
        reorder_query_joins(source, statistics, control, output)?;
    }
    for subquery in &mut plan.subqueries {
        reorder_query_joins(subquery, statistics, control, output)?;
    }
    Ok(())
}

fn reorder_expression_subquery_joins(
    expressions: &mut [ExpressionPlan],
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
    output: &mut Output,
) -> OptimizerResult<()> {
    for expression in expressions {
        for subquery in &mut expression.subqueries {
            reorder_query_joins(subquery, statistics, control, output)?;
        }
    }
    Ok(())
}

fn reorder_source_joins(
    source: &mut SourcePlan,
    external_predicates: &[&ScalarExpr],
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
    output: &mut Output,
) -> OptimizerResult<()> {
    match source {
        SourcePlan::RetainedRows(_) => {}
        SourcePlan::Join { left, right, .. } => {
            reorder_source_joins(left, &[], statistics, control, output)?;
            reorder_source_joins(right, &[], statistics, control, output)?;
        }
        SourcePlan::Subquery { body, .. } => {
            reorder_query_joins(body, statistics, control, output)?;
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }

    if let Some(reordered) =
        rewrite_inner_join_source(source, external_predicates, statistics, control, output)?
    {
        *source = reordered;
    }
    Ok(())
}

#[derive(Debug)]
struct JoinPredicateBinding {
    expression: Option<ScalarExpr>,
    relations: u64,
}

fn rewrite_inner_join_source(
    source: &mut SourcePlan,
    external_predicates: &[&ScalarExpr],
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
    output: &mut Output,
) -> OptimizerResult<Option<SourcePlan>> {
    let Some(selection::SelectedGraph {
        tree: selected,
        masks,
        inferred,
        original_count,
    }) = selection::select_graph(source, external_predicates, statistics, control)?
    else {
        return Ok(None);
    };
    let mut predicates = PredicateBindings::new(masks.len(), control)?;
    for relations in &*masks {
        predicates.push(JoinPredicateBinding {
            expression: None,
            relations: *relations,
        });
    }
    drop(masks);
    for (slot, expression) in predicates.as_mut_slice()[original_count..]
        .iter_mut()
        .zip(inferred.iter())
    {
        control.check().map_err(uqa_sql::SQLError::from)?;
        slot.expression = Some(
            output.retain_predicate(
                expression
                    .clone_with_control(control)
                    .map_err(uqa_sql::SQLError::from)?,
            ),
        );
    }
    let attachments =
        PredicateAttachments::prepare(&selected, predicates.as_mut_slice(), output, control)?;
    drop(inferred);
    let mut reordered =
        materialization::reorder(source, &selected, predicates.as_mut_slice(), output)?;
    attachments.attach(&mut reordered, &selected, predicates.as_mut_slice(), output);
    let remaining = predicates
        .as_slice()
        .iter()
        .filter(|predicate| predicate.expression.is_some())
        .count();
    if remaining != 0 {
        return Err(JoinGraphError::InvalidPlan {
            detail: format!("join reordering failed to retain {remaining} predicate(s)"),
        }
        .into());
    }
    Ok(Some(reordered))
}

fn copy_local_conjunction(
    predicates: &[&ScalarExpr],
    control: &ProductionControl<'_>,
) -> Result<Produced<ScalarExpr>, uqa_sql::SQLError> {
    let mut expressions = ProductionVec::new(*control);
    expressions.reserve(predicates.len())?;
    for predicate in predicates {
        expressions.push_produced(predicate.clone_with_control(control)?)?;
    }
    let (expressions, memory) = expressions.finish()?.into_parts();
    Ok(control.finish(ScalarExpr::And(expressions), memory)?)
}

fn estimate_local_filters(
    relations: &[RelationMetadata<'_>],
    predicates: &[&ScalarExpr],
    column_owners: &ColumnOwners<'_>,
    relation_tables: &[Option<&str>],
    source_statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<[Option<crate::LocalAccessEstimate>; 64]> {
    let estimator = crate::CardinalityEstimator::new();
    let mut estimates = [None; 64];
    let predicates_by_alias =
        local_predicate_groups(relations, predicates, column_owners, control)?;
    for (((relation, predicates), table), output) in relations
        .iter()
        .zip(predicates_by_alias)
        .zip(relation_tables)
        .zip(&mut estimates)
    {
        let predicates = predicates.expect("one predicate bucket per relation");
        if predicates.is_empty() {
            continue;
        }
        let conjunction;
        let predicate = if let [predicate] = &*predicates {
            *predicate
        } else {
            conjunction = copy_local_conjunction(&predicates, control)?;
            &conjunction
        };
        let statistics = crate::cardinality::RelationStatsRef {
            row_count: relation.cardinality.round() as u64,
            columns: &relation.column_stats,
        };
        let local_estimate =
            table.and_then(|table| source_statistics.local_access_estimate(table, predicate));
        if let Some(estimate) = local_estimate {
            if !estimate.output_rows.is_finite() || estimate.output_rows < 0.0 {
                return Err(JoinGraphError::InvalidCardinality {
                    name: relation.alias.to_string(),
                    rows: estimate.output_rows,
                }
                .into());
            }
            if !estimate.cost.is_finite() || estimate.cost < 0.0 {
                return Err(JoinGraphError::InvalidAccessCost {
                    name: relation.alias.to_string(),
                    cost: estimate.cost,
                }
                .into());
            }
            *output = Some(crate::LocalAccessEstimate {
                output_rows: estimate.output_rows.min(relation.cardinality).max(0.0),
                ..estimate
            });
            continue;
        }
        let selectivity = estimator
            .scalar_selectivity_with_statistics(predicate, statistics, control)?
            .raw();
        *output = Some(crate::LocalAccessEstimate {
            output_rows: (relation.cardinality * selectivity).max(0.0),
            cost: relation.access_cost
                + crate::CostEstimator::default()
                    .estimate_unary(crate::OperatorKind::Filter, statistics.row_count as f64)
                    .total(),
            paradigm: relation.paradigm,
        });
    }
    Ok(estimates)
}
