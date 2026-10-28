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
mod inference;
mod inputs;
use bindings::{attach_join_predicates, PredicateBindings};
use inference::{implied_join_predicates, join_predicate};
mod materialization;
mod relations;
use inputs::{
    collect_conjuncts, flatten_reorderable_inner_join, local_predicate_groups, ColumnOwners,
};
use relations::RelationMetadata;

pub(super) fn reorder_unified_plan_joins(
    plan: &mut UnifiedPlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    match plan {
        UnifiedPlan::Query(query) => reorder_query_joins(query, statistics, control),
        UnifiedPlan::Command(command) => reorder_command_joins(command, statistics, control),
    }
}

fn reorder_query_joins(
    query: &mut QueryPlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    for cte in &mut query.ctes {
        match &mut cte.body {
            crate::CtePlanBody::Query(query) => reorder_query_joins(query, statistics, control)?,
            crate::CtePlanBody::Command(command) => {
                reorder_command_joins(command, statistics, control)?;
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
                reorder_source_joins(source, &where_predicates, statistics, control)?;
            }
            for subquery in &mut block.subqueries {
                reorder_query_joins(subquery, statistics, control)?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            reorder_query_joins(left, statistics, control)?;
            reorder_query_joins(right, statistics, control)?;
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                reorder_query_joins(subquery, statistics, control)?;
            }
        }
    }
    Ok(())
}

fn reorder_cte_joins(
    cte: &mut crate::CtePlan,
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    match &mut cte.body {
        crate::CtePlanBody::Query(query) => reorder_query_joins(query, statistics, control),
        crate::CtePlanBody::Command(command) => reorder_command_joins(command, statistics, control),
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
) -> OptimizerResult<()> {
    match command {
        CommandPlan::Insert(plan) => reorder_insert_joins(plan, statistics, control)?,
        CommandPlan::Update(plan) => {
            for cte in &mut plan.ctes {
                reorder_cte_joins(cte, statistics, control)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_source_joins(source, &[], statistics, control)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics, control)?;
            }
        }
        CommandPlan::Delete(plan) => {
            for cte in &mut plan.ctes {
                reorder_cte_joins(cte, statistics, control)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_source_joins(source, &[], statistics, control)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics, control)?;
            }
        }
        CommandPlan::Merge(plan) => {
            for cte in &mut plan.ctes {
                reorder_cte_joins(cte, statistics, control)?;
            }
            reorder_source_joins(&mut plan.source, &[], statistics, control)?;
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics, control)?;
            }
        }
        CommandPlan::DeclareCursor { query, .. } => {
            reorder_query_joins(query, statistics, control)?;
        }
        CommandPlan::Call { args: params, .. } => {
            reorder_expression_subquery_joins(params, statistics, control)?;
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
) -> OptimizerResult<()> {
    for cte in &mut plan.ctes {
        match &mut cte.body {
            crate::CtePlanBody::Query(query) => reorder_query_joins(query, statistics, control)?,
            crate::CtePlanBody::Command(command) => {
                reorder_command_joins(command, statistics, control)?;
            }
        }
    }
    if let Some(source) = &mut plan.source {
        reorder_query_joins(source, statistics, control)?;
    }
    for subquery in &mut plan.subqueries {
        reorder_query_joins(subquery, statistics, control)?;
    }
    Ok(())
}

fn reorder_expression_subquery_joins(
    expressions: &mut [ExpressionPlan],
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    for expression in expressions {
        for subquery in &mut expression.subqueries {
            reorder_query_joins(subquery, statistics, control)?;
        }
    }
    Ok(())
}

fn reorder_source_joins(
    source: &mut SourcePlan,
    external_predicates: &[&ScalarExpr],
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<()> {
    match source {
        SourcePlan::RetainedRows(_) => {}
        SourcePlan::Join { left, right, .. } => {
            reorder_source_joins(left, &[], statistics, control)?;
            reorder_source_joins(right, &[], statistics, control)?;
        }
        SourcePlan::Subquery { body, .. } => reorder_query_joins(body, statistics, control)?,
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }

    if let Some(reordered) =
        reordered_inner_join_source(source, external_predicates, statistics, control)?
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

#[expect(
    clippy::too_many_lines,
    reason = "join rewrite preserves predicate ownership and relation order atomically"
)]
fn reordered_inner_join_source(
    source: &mut SourcePlan,
    external_predicates: &[&ScalarExpr],
    statistics: &dyn SourceStatistics,
    control: &ProductionControl<'_>,
) -> OptimizerResult<Option<SourcePlan>> {
    let mut atoms = ProductionSmallVec::<[&SourcePlan; 64]>::new(*control);
    let mut expressions = ProductionVec::new(*control);
    if !flatten_reorderable_inner_join(source, &mut atoms, &mut expressions, control)?
        || !(2..=64).contains(&atoms.len())
    {
        return Ok(None);
    }

    let mut relation_storage: [RelationMetadata<'_>; 64] =
        std::array::from_fn(|_| RelationMetadata::default());
    let relations = &mut relation_storage[..atoms.len()];
    let mut relation_tables = [None; 64];
    for (source_id, atom) in atoms.iter().enumerate() {
        let (qualifier, cardinality, column_stats, access_cost, paradigm) = match atom {
            SourcePlan::RetainedRows(_) => return Ok(None),
            SourcePlan::Table {
                name,
                qualifier,
                alias,
                ..
            } => {
                let qualifier = alias.as_deref().unwrap_or(qualifier);
                relation_tables[source_id] = Some(name.as_str());
                let relation_stats = statistics.relation_statistics(name);
                let cardinality = relation_stats
                    .as_ref()
                    .map_or(DEFAULT_JOIN_CARDINALITY, |stats| stats.row_count)
                    as f64;
                let column_stats = relation_stats
                    .map_or_else(crate::ColumnStatistics::default, |stats| stats.columns);
                let access_cost = crate::CostEstimator::default()
                    .estimate_unary(crate::OperatorKind::TableScan, cardinality)
                    .total();
                (
                    qualifier,
                    cardinality,
                    column_stats,
                    access_cost,
                    crate::AccessParadigm::Relational,
                )
            }
            SourcePlan::Function { .. } => {
                let Some(estimate) = statistics.source_access_estimate(atom) else {
                    return Ok(None);
                };
                (
                    atom.visible_qualifier()
                        .expect("a function source always has a visible qualifier"),
                    estimate.output_rows,
                    crate::ColumnStatistics::default(),
                    estimate.cost,
                    estimate.paradigm,
                )
            }
            SourcePlan::Join { .. }
            | SourcePlan::Values { .. }
            | SourcePlan::FunctionGroup { .. }
            | SourcePlan::Subquery { .. } => return Ok(None),
        };
        if qualifier.is_empty()
            || relations[..source_id]
                .iter()
                .any(|relation| relation.alias == qualifier)
        {
            return Ok(None);
        }
        relations[source_id] = RelationMetadata {
            alias: qualifier.into(),
            cardinality,
            column_stats,
            access_cost,
            paradigm,
        };
    }
    let column_owners = ColumnOwners::new(relations);
    let local_estimates = estimate_local_filters(
        relations,
        external_predicates,
        &column_owners,
        &relation_tables,
        statistics,
        control,
    )?;

    // SQL's comma-separated FROM form lowers to a tree of cross joins while
    // its join equalities remain in the query-block WHERE expression. Treat
    // only top-level conjunctive column equalities as join-graph edges. The
    // WHERE expression stays in place as a post-join semantic guard; adding
    // the same equality to the hash join changes evaluation shape, not rows.
    let mut inferred = ProductionVec::new(*control);
    for expression in external_predicates {
        let decisions = implied_join_predicates(expression, &column_owners, control)?;
        for (_, decision) in &decisions {
            inferred
                .push_copy(*decision)
                .map_err(uqa_sql::SQLError::from)?;
        }
    }

    let original_count = expressions.len();
    let count = original_count
        .checked_add(inferred.len())
        .ok_or(uqa_core::memory::MemoryError::SizeOverflow)
        .map_err(uqa_sql::SQLError::from)?;
    let mut predicates = PredicateBindings::new(count, control)?;
    let mut graph_predicates = ProductionVec::new(*control);
    for expression in expressions.iter().copied().chain(inferred.iter().copied()) {
        control.check().map_err(uqa_sql::SQLError::from)?;
        let graph_predicate = join_predicate(expression, &column_owners);
        let mut relations = 0;
        if let Some(predicate) = graph_predicate {
            for alias in [predicate.left_alias, predicate.right_alias] {
                relations |= 1_u64
                    << column_owners
                        .alias_index(alias)
                        .expect("resolved join alias");
            }
            graph_predicates
                .push_copy(predicate)
                .map_err(uqa_sql::SQLError::from)?;
        }
        predicates.push(JoinPredicateBinding {
            expression: None,
            relations,
        });
    }

    let selected = JoinOrderOptimizer::new().select_relations_with_control(
        relations.iter().enumerate().map(|(index, relation)| {
            let mut input = relation.as_join_relation();
            if let Some(estimate) = local_estimates[index] {
                input.cardinality = estimate.output_rows;
                input.access_cost = estimate.cost;
            }
            input
        }),
        graph_predicates.iter().copied(),
        control,
    )?;
    for (slot, expression) in predicates.as_mut_slice()[original_count..]
        .iter_mut()
        .zip(inferred.iter())
    {
        control.check().map_err(uqa_sql::SQLError::from)?;
        slot.expression = Some((*expression).clone());
    }
    drop(graph_predicates);
    drop(inferred);
    drop(expressions);
    drop(atoms);
    drop(relation_storage);
    let mut reordered = materialization::reorder(source, &selected, predicates.as_mut_slice())?;
    attach_join_predicates(&mut reordered, &selected, predicates.as_mut_slice(), true);
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
