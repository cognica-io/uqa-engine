//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Plan-native algebraic and cross-paradigm rewrites.
//!
//! Parsing is complete before this module runs. The optimizer walks every
//! relational, scalar, mutation, CTE, set-operation, and query-valued command
//! child in a [`UnifiedPlan`]. It never reconstructs a SQL AST.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::Value;
use uqa_execution::{ScalarExpr, ScalarFrameBound};
use uqa_sql::ast::BinaryOp;

use crate::unified_plan::{
    AccessPathPlan, AggregateClassifier, AssignmentPlan, CommandPlan, ComputePlan,
    ConflictActionPlan, ExpressionPlan, JoinExecutionStrategy, MergeWhenPlan, ProjectionPlan,
    QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};
use crate::{
    JoinAlgorithm, JoinGraphError, JoinGraphResult, JoinOrderOptimizer, JoinOrderTree,
    JoinPredicate, JoinRelation, RelationStats,
};

#[derive(Debug, Clone)]
pub struct OptimizerConfig {
    pub enable_filter_pushdown: bool,
    pub enable_boolean_simplify: bool,
    pub enable_vector_threshold_merge: bool,
    pub enable_join_reordering: bool,
}

impl Default for OptimizerConfig {
    fn default() -> Self {
        Self {
            enable_filter_pushdown: true,
            enable_boolean_simplify: true,
            enable_vector_threshold_merge: true,
            enable_join_reordering: true,
        }
    }
}

/// Cardinality and column statistics used to cost base relations during
/// join enumeration. Engines implement this against their live catalogue;
/// callers without a catalogue still get deterministic DPccp enumeration
/// from the optimizer's fallback cardinality.
pub trait SourceStatistics {
    fn relation_statistics(&self, table: &str) -> Option<RelationStats>;
}

impl<F> SourceStatistics for F
where
    F: Fn(&str) -> Option<RelationStats>,
{
    fn relation_statistics(&self, table: &str) -> Option<RelationStats> {
        self(table)
    }
}

struct NoSourceStatistics;

impl SourceStatistics for NoSourceStatistics {
    fn relation_statistics(&self, _table: &str) -> Option<RelationStats> {
        None
    }
}

struct NoRegisteredAggregates;

impl AggregateClassifier for NoRegisteredAggregates {
    fn is_registered_aggregate(&self, _name: &str) -> bool {
        false
    }
}

/// Optimize a fully lowered plan using the built-in aggregate catalogue.
pub fn optimize(plan: UnifiedPlan, config: &OptimizerConfig) -> JoinGraphResult<UnifiedPlan> {
    optimize_with_aggregates_and_statistics(
        plan,
        config,
        &NoRegisteredAggregates,
        &NoSourceStatistics,
    )
}

/// Optimize a fully lowered plan while classifying engine-local aggregates.
pub fn optimize_with_aggregates(
    plan: UnifiedPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) -> JoinGraphResult<UnifiedPlan> {
    optimize_with_aggregates_and_statistics(plan, config, aggregates, &NoSourceStatistics)
}

/// Optimize a fully lowered plan with caller-provided relation statistics.
pub fn optimize_with_statistics(
    plan: UnifiedPlan,
    config: &OptimizerConfig,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<UnifiedPlan> {
    optimize_with_aggregates_and_statistics(plan, config, &NoRegisteredAggregates, statistics)
}

/// Optimize a fully lowered plan while classifying engine-local aggregates
/// and costing join orders from the engine's relation statistics.
pub fn optimize_with_aggregates_and_statistics(
    mut plan: UnifiedPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<UnifiedPlan> {
    optimize_unified_plan(&mut plan, config, aggregates);
    if config.enable_join_reordering {
        reorder_unified_plan_joins(&mut plan, statistics)?;
    }
    Ok(plan)
}

fn optimize_unified_plan(
    plan: &mut UnifiedPlan,
    config: &OptimizerConfig,
    aggregates: &dyn AggregateClassifier,
) {
    match plan {
        UnifiedPlan::Query(query) => optimize_query(query, config, aggregates),
        UnifiedPlan::Command(command) => optimize_command(command, config, aggregates),
    }
}

fn optimize_query(
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

fn optimize_query_block(
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
        super::unified_plan::is_builtin_aggregate(name) || aggregates.is_registered_aggregate(name)
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

fn optimize_source(
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
        SourcePlan::Subquery { body, .. } => optimize_query(body, config, aggregates),
        SourcePlan::Table { .. } => {}
    }
}

const DEFAULT_JOIN_CARDINALITY: u64 = 1_000;

fn reorder_unified_plan_joins(
    plan: &mut UnifiedPlan,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<()> {
    match plan {
        UnifiedPlan::Query(query) => reorder_query_joins(query, statistics),
        UnifiedPlan::Command(command) => reorder_command_joins(command, statistics),
    }
}

fn reorder_query_joins(
    query: &mut QueryPlan,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<()> {
    for cte in &mut query.ctes {
        reorder_query_joins(&mut cte.query, statistics)?;
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                reorder_source_joins(source, statistics)?;
            }
            for subquery in &mut block.subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            reorder_query_joins(left, statistics)?;
            reorder_query_joins(right, statistics)?;
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
    }
    Ok(())
}

fn reorder_command_joins(
    command: &mut CommandPlan,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<()> {
    match command {
        CommandPlan::Insert(plan) => {
            for cte in &mut plan.ctes {
                reorder_query_joins(&mut cte.query, statistics)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_query_joins(source, statistics)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
        CommandPlan::Update(plan) => {
            for cte in &mut plan.ctes {
                reorder_query_joins(&mut cte.query, statistics)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_source_joins(source, statistics)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
        CommandPlan::Delete(plan) => {
            for cte in &mut plan.ctes {
                reorder_query_joins(&mut cte.query, statistics)?;
            }
            if let Some(source) = &mut plan.source {
                reorder_source_joins(source, statistics)?;
            }
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
        CommandPlan::Merge(plan) => {
            reorder_source_joins(&mut plan.source, statistics)?;
            for subquery in &mut plan.subqueries {
                reorder_query_joins(subquery, statistics)?;
            }
        }
        CommandPlan::CreateView { query, .. } | CommandPlan::CreateTableAs { query, .. } => {
            reorder_query_joins(query, statistics)?;
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            reorder_unified_plan_joins(body, statistics)?;
        }
        CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
            for expression in params {
                for subquery in &mut expression.subqueries {
                    reorder_query_joins(subquery, statistics)?;
                }
            }
        }
        CommandPlan::CreateTable(_)
        | CommandPlan::CreateIndex(_)
        | CommandPlan::Drop(_)
        | CommandPlan::AlterTable(_)
        | CommandPlan::CreateSchema { .. }
        | CommandPlan::SetVariable { .. }
        | CommandPlan::ShowVariable { .. }
        | CommandPlan::Discard { .. }
        | CommandPlan::Analyze { .. }
        | CommandPlan::Truncate { .. }
        | CommandPlan::Transaction(_)
        | CommandPlan::CreateSequence(_)
        | CommandPlan::AlterSequence(_)
        | CommandPlan::Deallocate { .. }
        | CommandPlan::CreateForeignServer(_)
        | CommandPlan::CreateForeignTable(_)
        | CommandPlan::CreateFunction(_)
        | CommandPlan::DropFunction(_)
        | CommandPlan::DoBlock { .. } => {}
    }
    Ok(())
}

fn reorder_source_joins(
    source: &mut SourcePlan,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<()> {
    match source {
        SourcePlan::Join { left, right, .. } => {
            reorder_source_joins(left, statistics)?;
            reorder_source_joins(right, statistics)?;
        }
        SourcePlan::Subquery { body, .. } => reorder_query_joins(body, statistics)?,
        SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Function { .. } => {}
    }

    if let Some(reordered) = reordered_inner_join_source(source, statistics)? {
        *source = reordered;
    }
    Ok(())
}

#[derive(Debug)]
struct JoinPredicateBinding {
    expression: ScalarExpr,
    qualifiers: BTreeSet<String>,
    pushable: bool,
}

fn reordered_inner_join_source(
    source: &SourcePlan,
    statistics: &dyn SourceStatistics,
) -> JoinGraphResult<Option<SourcePlan>> {
    let mut atoms = Vec::new();
    let mut expressions = Vec::new();
    if !flatten_reorderable_inner_join(source, &mut atoms, &mut expressions)
        || !(2..=64).contains(&atoms.len())
    {
        return Ok(None);
    }

    let mut aliases = BTreeSet::new();
    let mut relations = Vec::with_capacity(atoms.len());
    for (source_id, atom) in atoms.iter().enumerate() {
        let SourcePlan::Table { name, alias } = atom else {
            return Ok(None);
        };
        let qualifier = alias
            .clone()
            .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(name).to_string());
        if qualifier.is_empty() || !aliases.insert(qualifier.clone()) {
            return Ok(None);
        }
        let relation_stats = statistics.relation_statistics(name);
        let cardinality = relation_stats
            .as_ref()
            .map_or(DEFAULT_JOIN_CARDINALITY, |stats| stats.row_count)
            as f64;
        let column_stats = relation_stats.map_or_else(BTreeMap::new, |stats| stats.columns);
        let source_id = u64::try_from(source_id).map_err(|_| JoinGraphError::InvalidPlan {
            detail: format!("source index {source_id} exceeds the join plan identifier range"),
        })?;
        relations.push(JoinRelation {
            alias: qualifier,
            cardinality,
            column_stats,
            source_id,
        });
    }

    let mut predicates = Vec::new();
    let mut graph_predicates = Vec::new();
    for expression in expressions {
        let mut qualifiers = BTreeSet::new();
        collect_scalar_qualifiers(&expression, &mut qualifiers);
        let graph_predicate = join_predicate(&expression, &aliases);
        let pushable = graph_predicate.is_some();
        graph_predicates.extend(graph_predicate);
        predicates.push(JoinPredicateBinding {
            expression,
            qualifiers,
            pushable,
        });
    }

    let result = JoinOrderOptimizer::new().optimize(relations, graph_predicates)?;
    let mut reordered = materialize_join_order(result.tree, &atoms)?;
    attach_join_predicates(&mut reordered, &mut predicates, true);
    if !predicates.is_empty() {
        return Err(JoinGraphError::InvalidPlan {
            detail: format!(
                "join reordering failed to retain {} predicate(s)",
                predicates.len()
            ),
        });
    }
    Ok(Some(reordered))
}

fn flatten_reorderable_inner_join(
    source: &SourcePlan,
    atoms: &mut Vec<SourcePlan>,
    predicates: &mut Vec<ScalarExpr>,
) -> bool {
    match source {
        SourcePlan::Join {
            left,
            right,
            kind: uqa_sql::ast::JoinKind::Inner,
            on,
            lateral: false,
            strategy: _,
        } => {
            if !flatten_reorderable_inner_join(left, atoms, predicates)
                || !flatten_reorderable_inner_join(right, atoms, predicates)
            {
                return false;
            }
            if let Some(on) = on {
                collect_conjuncts(on, predicates);
            }
            true
        }
        SourcePlan::Table { .. } => {
            atoms.push(source.clone());
            true
        }
        SourcePlan::Join { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::Subquery { .. } => false,
    }
}

fn collect_conjuncts(expression: &ScalarExpr, output: &mut Vec<ScalarExpr>) {
    if let ScalarExpr::And(items) = expression {
        for item in items {
            collect_conjuncts(item, output);
        }
    } else {
        output.push(expression.clone());
    }
}

fn join_predicate(expression: &ScalarExpr, aliases: &BTreeSet<String>) -> Option<JoinPredicate> {
    let ScalarExpr::Binary {
        op: BinaryOp::Equal,
        lhs,
        rhs,
    } = expression
    else {
        return None;
    };
    let (
        ScalarExpr::QualifiedColumn {
            qualifier: left_alias,
            column: left_field,
            ..
        },
        ScalarExpr::QualifiedColumn {
            qualifier: right_alias,
            column: right_field,
            ..
        },
    ) = (lhs.as_ref(), rhs.as_ref())
    else {
        return None;
    };
    if left_alias == right_alias || !aliases.contains(left_alias) || !aliases.contains(right_alias)
    {
        return None;
    }
    Some(JoinPredicate {
        left_alias: left_alias.clone(),
        right_alias: right_alias.clone(),
        left_field: left_field.clone(),
        right_field: right_field.clone(),
    })
}

fn materialize_join_order(
    tree: JoinOrderTree,
    atoms: &[SourcePlan],
) -> JoinGraphResult<SourcePlan> {
    match tree {
        JoinOrderTree::Scan(relation) => {
            let source_id =
                usize::try_from(relation.source_id).map_err(|_| JoinGraphError::InvalidPlan {
                    detail: format!(
                        "join source id {} exceeds the addressable source range",
                        relation.source_id
                    ),
                })?;
            atoms
                .get(source_id)
                .cloned()
                .ok_or_else(|| JoinGraphError::InvalidPlan {
                    detail: format!(
                        "join source id {source_id} is outside {} source atom(s)",
                        atoms.len()
                    ),
                })
        }
        JoinOrderTree::Inner {
            algorithm,
            left,
            right,
            ..
        } => Ok(SourcePlan::Join {
            left: Box::new(materialize_join_order(*left, atoms)?),
            right: Box::new(materialize_join_order(*right, atoms)?),
            kind: uqa_sql::ast::JoinKind::Inner,
            on: None,
            lateral: false,
            strategy: match algorithm {
                JoinAlgorithm::Hash => JoinExecutionStrategy::Hash,
            },
        }),
        JoinOrderTree::Cross { left, right } => Ok(SourcePlan::Join {
            left: Box::new(materialize_join_order(*left, atoms)?),
            right: Box::new(materialize_join_order(*right, atoms)?),
            kind: uqa_sql::ast::JoinKind::Inner,
            on: None,
            lateral: false,
            strategy: JoinExecutionStrategy::Auto,
        }),
    }
}

fn attach_join_predicates(
    source: &mut SourcePlan,
    predicates: &mut Vec<JoinPredicateBinding>,
    root: bool,
) -> BTreeSet<String> {
    let SourcePlan::Join {
        left, right, on, ..
    } = source
    else {
        return source_qualifiers(source);
    };

    let mut available = attach_join_predicates(left, predicates, false);
    available.extend(attach_join_predicates(right, predicates, false));

    let mut assigned = Vec::new();
    let mut retained = Vec::new();
    for predicate in std::mem::take(predicates) {
        if root
            || (predicate.pushable
                && !predicate.qualifiers.is_empty()
                && predicate.qualifiers.is_subset(&available))
        {
            assigned.push(predicate.expression);
        } else {
            retained.push(predicate);
        }
    }
    *predicates = retained;
    *on = match assigned.len() {
        0 => None,
        1 => assigned.pop(),
        _ => Some(ScalarExpr::And(assigned)),
    };
    available
}

fn source_qualifiers(source: &SourcePlan) -> BTreeSet<String> {
    let mut qualifiers = BTreeSet::new();
    if let SourcePlan::Table { name, alias } = source {
        qualifiers.insert(
            alias
                .clone()
                .unwrap_or_else(|| name.rsplit('.').next().unwrap_or(name).to_string()),
        );
    }
    qualifiers
}

fn collect_scalar_qualifiers(expression: &ScalarExpr, output: &mut BTreeSet<String>) {
    match expression {
        ScalarExpr::QualifiedColumn { qualifier, .. } => {
            output.insert(qualifier.clone());
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_scalar_qualifiers(argument, output);
            }
            for order in order_by {
                collect_scalar_qualifiers(&order.expr, output);
            }
            if let Some(filter) = filter {
                collect_scalar_qualifiers(filter, output);
            }
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                collect_scalar_qualifiers(item, output);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_scalar_qualifiers(lhs, output);
            collect_scalar_qualifiers(rhs, output);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => collect_scalar_qualifiers(inner, output),
        ScalarExpr::Between { expr, low, high } => {
            collect_scalar_qualifiers(expr, output);
            collect_scalar_qualifiers(low, output);
            collect_scalar_qualifiers(high, output);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_scalar_qualifiers(expr, output);
            for item in list {
                collect_scalar_qualifiers(item, output);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_scalar_qualifiers(argument, output);
            }
            for partition in &spec.partition_by {
                collect_scalar_qualifiers(partition, output);
            }
            for order in &spec.order_by {
                collect_scalar_qualifiers(&order.expr, output);
            }
            if let Some(frame) = &spec.frame {
                collect_frame_bound_qualifiers(&frame.start, output);
                collect_frame_bound_qualifiers(&frame.end, output);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_scalar_qualifiers(base, output);
            }
            for (condition, result) in when {
                collect_scalar_qualifiers(condition, output);
                collect_scalar_qualifiers(result, output);
            }
            if let Some(branch) = else_branch {
                collect_scalar_qualifiers(branch, output);
            }
        }
        ScalarExpr::InSubquery { expr, .. } => collect_scalar_qualifiers(expr, output),
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
}

fn collect_frame_bound_qualifiers(bound: &ScalarFrameBound, output: &mut BTreeSet<String>) {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            collect_scalar_qualifiers(expression, output);
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
}

fn optimize_command(
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
                    } => {
                        if let Some(condition) = condition {
                            optimize_scalar_slot(condition, config);
                        }
                        optimize_assignments(assignments, config);
                    }
                    MergeWhenPlan::DeleteMatched { condition }
                    | MergeWhenPlan::NothingMatched { condition }
                    | MergeWhenPlan::NothingNotMatched { condition } => {
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
        CommandPlan::CreateView { query, .. } | CommandPlan::CreateTableAs { query, .. } => {
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
        | CommandPlan::CreateSchema { .. }
        | CommandPlan::SetVariable { .. }
        | CommandPlan::ShowVariable { .. }
        | CommandPlan::Discard { .. }
        | CommandPlan::Analyze { .. }
        | CommandPlan::Truncate { .. }
        | CommandPlan::Transaction(_)
        | CommandPlan::CreateSequence(_)
        | CommandPlan::AlterSequence(_)
        | CommandPlan::Deallocate { .. }
        | CommandPlan::CreateForeignServer(_)
        | CommandPlan::CreateForeignTable(_)
        | CommandPlan::CreateFunction(_)
        | CommandPlan::DropFunction(_)
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

fn optimize_assignments(assignments: &mut [AssignmentPlan], config: &OptimizerConfig) {
    for assignment in assignments {
        optimize_scalar_slot(&mut assignment.value, config);
    }
}

fn optimize_projections(projections: &mut [ProjectionPlan], config: &OptimizerConfig) {
    for projection in projections {
        optimize_scalar_slot(&mut projection.expr, config);
    }
}

fn optimize_scalar_slot(expression: &mut ScalarExpr, config: &OptimizerConfig) {
    let placeholder = ScalarExpr::Literal(Value::Null);
    let mut optimized = optimize_scalar(std::mem::replace(expression, placeholder), config);
    if config.enable_vector_threshold_merge {
        optimized = merge_vector_thresholds(optimized);
    }
    *expression = optimized;
}

fn optimize_scalar(expression: ScalarExpr, config: &OptimizerConfig) -> ScalarExpr {
    match expression {
        ScalarExpr::Array(items) => ScalarExpr::Array(
            items
                .into_iter()
                .map(|item| optimize_scalar(item, config))
                .collect(),
        ),
        ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op,
            lhs: Box::new(optimize_scalar(*lhs, config)),
            rhs: Box::new(optimize_scalar(*rhs, config)),
        },
        ScalarExpr::Not(inner) => {
            let inner = optimize_scalar(*inner, config);
            if config.enable_boolean_simplify {
                match inner {
                    ScalarExpr::Literal(Value::Bool(value)) => {
                        ScalarExpr::Literal(Value::Bool(!value))
                    }
                    ScalarExpr::Not(inner) => *inner,
                    other => ScalarExpr::Not(Box::new(other)),
                }
            } else {
                ScalarExpr::Not(Box::new(inner))
            }
        }
        ScalarExpr::And(items) => {
            let items = items
                .into_iter()
                .map(|item| optimize_scalar(item, config))
                .collect();
            if config.enable_boolean_simplify {
                simplify_and(items)
            } else {
                ScalarExpr::And(items)
            }
        }
        ScalarExpr::Or(items) => {
            let items = items
                .into_iter()
                .map(|item| optimize_scalar(item, config))
                .collect();
            if config.enable_boolean_simplify {
                simplify_or(items)
            } else {
                ScalarExpr::Or(items)
            }
        }
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(optimize_scalar(*expr, config)),
            negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(optimize_scalar(*expr, config)),
            low: Box::new(optimize_scalar(*low, config)),
            high: Box::new(optimize_scalar(*high, config)),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(optimize_scalar(*expr, config)),
            list: list
                .into_iter()
                .map(|item| optimize_scalar(item, config))
                .collect(),
            negated,
        },
        ScalarExpr::Func {
            name,
            args,
            distinct,
            mut order_by,
            filter,
        } => {
            for order in &mut order_by {
                order.expr = optimize_scalar(
                    std::mem::replace(&mut order.expr, ScalarExpr::Literal(Value::Null)),
                    config,
                );
            }
            ScalarExpr::Func {
                name,
                args: args
                    .into_iter()
                    .map(|argument| optimize_scalar(argument, config))
                    .collect(),
                distinct,
                order_by,
                filter: filter.map(|filter| Box::new(optimize_scalar(*filter, config))),
            }
        }
        ScalarExpr::WindowCall {
            name,
            args,
            mut spec,
        } => {
            spec.partition_by = spec
                .partition_by
                .into_iter()
                .map(|expression| optimize_scalar(expression, config))
                .collect();
            for order in &mut spec.order_by {
                order.expr = optimize_scalar(
                    std::mem::replace(&mut order.expr, ScalarExpr::Literal(Value::Null)),
                    config,
                );
            }
            if let Some(frame) = &mut spec.frame {
                optimize_frame_bound(&mut frame.start, config);
                optimize_frame_bound(&mut frame.end, config);
            }
            ScalarExpr::WindowCall {
                name,
                args: args
                    .into_iter()
                    .map(|argument| optimize_scalar(argument, config))
                    .collect(),
                spec,
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: base.map(|base| Box::new(optimize_scalar(*base, config))),
            when: when
                .into_iter()
                .map(|(condition, result)| {
                    (
                        optimize_scalar(condition, config),
                        optimize_scalar(result, config),
                    )
                })
                .collect(),
            else_branch: else_branch.map(|branch| Box::new(optimize_scalar(*branch, config))),
        },
        ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(optimize_scalar(*expr, config)),
            ty,
        },
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(optimize_scalar(*expr, config)),
            subquery,
            negated,
        },
        other => other,
    }
}

fn optimize_frame_bound(bound: &mut ScalarFrameBound, config: &OptimizerConfig) {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            optimize_scalar_slot(expression, config);
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
}

fn simplify_and(items: Vec<ScalarExpr>) -> ScalarExpr {
    let mut kept = Vec::new();
    for item in items {
        match item {
            ScalarExpr::Literal(Value::Bool(true)) => {}
            ScalarExpr::Literal(Value::Bool(false)) => {
                return ScalarExpr::Literal(Value::Bool(false));
            }
            ScalarExpr::And(inner) => kept.extend(inner),
            other => kept.push(other),
        }
    }
    if kept.is_empty() {
        ScalarExpr::Literal(Value::Bool(true))
    } else if kept.len() == 1 {
        kept.remove(0)
    } else {
        ScalarExpr::And(kept)
    }
}

fn simplify_or(items: Vec<ScalarExpr>) -> ScalarExpr {
    let mut kept = Vec::new();
    for item in items {
        match item {
            ScalarExpr::Literal(Value::Bool(false)) => {}
            ScalarExpr::Literal(Value::Bool(true)) => {
                return ScalarExpr::Literal(Value::Bool(true));
            }
            ScalarExpr::Or(inner) => kept.extend(inner),
            other => kept.push(other),
        }
    }
    if kept.is_empty() {
        ScalarExpr::Literal(Value::Bool(false))
    } else if kept.len() == 1 {
        kept.remove(0)
    } else {
        ScalarExpr::Or(kept)
    }
}

fn merge_vector_thresholds(expression: ScalarExpr) -> ScalarExpr {
    match expression {
        ScalarExpr::And(items) => {
            let mut by_field: BTreeMap<String, (ScalarExpr, f64)> = BTreeMap::new();
            let mut others = Vec::new();
            for item in items {
                if let ScalarExpr::Func { name, args, .. } = &item {
                    if name.eq_ignore_ascii_case("knn_match") && args.len() >= 3 {
                        if let (
                            ScalarExpr::Literal(Value::Str(field)),
                            ScalarExpr::Literal(Value::Float(threshold)),
                        ) = (&args[0], &args[2])
                        {
                            let entry = by_field
                                .entry(field.clone())
                                .or_insert_with(|| (item.clone(), *threshold));
                            if *threshold > entry.1 {
                                entry.1 = *threshold;
                                if let ScalarExpr::Func { args, .. } = &mut entry.0 {
                                    args[2] = ScalarExpr::Literal(Value::Float(*threshold));
                                }
                            }
                            continue;
                        }
                    }
                }
                others.push(merge_vector_thresholds(item));
            }
            others.extend(by_field.into_values().map(|(expression, _)| expression));
            if others.is_empty() {
                ScalarExpr::Literal(Value::Bool(true))
            } else if others.len() == 1 {
                others.remove(0)
            } else {
                ScalarExpr::And(others)
            }
        }
        ScalarExpr::Or(items) => {
            ScalarExpr::Or(items.into_iter().map(merge_vector_thresholds).collect())
        }
        ScalarExpr::Not(inner) => ScalarExpr::Not(Box::new(merge_vector_thresholds(*inner))),
        other => other,
    }
}

/// Put posting-list-compatible conjuncts before row residuals. The executor
/// can then build the smallest candidate set before touching documents.
fn prioritize_access_predicates(expression: &mut ScalarExpr) {
    if let ScalarExpr::And(items) = expression {
        for item in items.iter_mut() {
            prioritize_access_predicates(item);
        }
        let mut access = Vec::with_capacity(items.len());
        let mut residual = Vec::new();
        for item in std::mem::take(items) {
            if operator_compatible(&item) {
                access.push(item);
            } else {
                residual.push(item);
            }
        }
        access.extend(residual);
        *items = access;
    }
}

fn choose_access_path(block: &QueryBlockPlan) -> AccessPathPlan {
    if !matches!(block.from, Some(SourcePlan::Table { .. })) {
        return AccessPathPlan::Row;
    }
    let Some(predicate) = block.r#where.as_ref() else {
        return AccessPathPlan::Row;
    };
    if operator_compatible(predicate) {
        let score_limit_pushdown = block.limit.is_some()
            && root_score_retrieval(predicate)
            && !block.order_by.is_empty()
            && block.order_by.iter().all(|order| {
                order.descending
                    && matches!(
                        &order.expr,
                        ScalarExpr::Column(name)
                            | ScalarExpr::QualifiedColumn { column: name, .. }
                            if name == "_score"
                    )
            });
        AccessPathPlan::OperatorTree {
            score_limit_pushdown,
        }
    } else if contains_retrieval(predicate) {
        AccessPathPlan::Hybrid
    } else {
        AccessPathPlan::Row
    }
}

fn root_score_retrieval(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Func { name, .. }
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "text_match" | "bayesian_match" | "fts_match" | "bayesian_match_with_prior"
            )
    )
}

/// Whether a scalar expression contains a posting-list retrieval operator.
/// Relational executors use the same classification as access-path planning
/// so registered retrieval calls never fall through to scalar evaluation.
pub fn contains_retrieval(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Func {
            name,
            args,
            order_by,
            filter,
            ..
        } => {
            retrieval_function(name)
                || args.iter().any(contains_retrieval)
                || order_by.iter().any(|order| contains_retrieval(&order.expr))
                || filter.as_deref().is_some_and(contains_retrieval)
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(contains_retrieval)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => contains_retrieval(lhs) || contains_retrieval(rhs),
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => contains_retrieval(inner),
        ScalarExpr::Between { expr, low, high } => {
            contains_retrieval(expr) || contains_retrieval(low) || contains_retrieval(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            contains_retrieval(expr) || list.iter().any(contains_retrieval)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter().any(contains_retrieval)
                || spec.partition_by.iter().any(contains_retrieval)
                || spec
                    .order_by
                    .iter()
                    .any(|order| contains_retrieval(&order.expr))
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(contains_retrieval)
                || when.iter().any(|(condition, result)| {
                    contains_retrieval(condition) || contains_retrieval(result)
                })
                || else_branch.as_deref().is_some_and(contains_retrieval)
        }
        ScalarExpr::InSubquery { expr, .. } => contains_retrieval(expr),
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

fn operator_compatible(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            !items.is_empty() && items.iter().all(operator_compatible)
        }
        ScalarExpr::Not(inner) => operator_compatible(inner),
        ScalarExpr::Binary { op, lhs, rhs } => {
            matches!(
                op,
                BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual
            ) && scalar_operand(lhs)
                && scalar_operand(rhs)
        }
        ScalarExpr::IsNull { expr, .. } => scalar_operand(expr),
        ScalarExpr::Between { expr, low, high } => {
            scalar_operand(expr) && scalar_operand(low) && scalar_operand(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            scalar_operand(expr) && list.iter().all(scalar_operand)
        }
        ScalarExpr::Func { name, .. } => retrieval_function(name),
        _ => false,
    }
}

fn scalar_operand(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Column(_)
            | ScalarExpr::QualifiedColumn { .. }
            | ScalarExpr::Literal(_)
            | ScalarExpr::Param(_)
    )
}

fn retrieval_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "text_match"
            | "bayesian_match"
            | "fts_match"
            | "bayesian_match_with_prior"
            | "calibrated_vector_match"
            | "knn_match"
            | "fuse_log_odds"
            | "multi_field_match"
            | "staged_retrieval"
            | "attention"
            | "fuse_attention"
            | "fuse_multihead"
            | "learned_fusion"
            | "fuse_learned"
            | "sparse_threshold"
            | "graph_pagerank"
            | "pagerank"
            | "graph_hits"
            | "hits"
            | "graph_betweenness"
            | "betweenness"
            | "graph_traverse"
            | "traverse_match"
            | "graph_neighbors"
            | "graph_edges"
            | "temporal_traverse"
            | "rpq"
            | "deep_predict"
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use uqa_sql::compile;

    use super::*;

    fn optimized(sql: &str) -> UnifiedPlan {
        let mut statements = compile(sql).expect("SQL compiles");
        optimize(
            UnifiedPlan::lower(statements.remove(0)),
            &OptimizerConfig::default(),
        )
        .expect("optimizer succeeds")
    }

    fn optimized_with_rows(sql: &str, rows: &[(&str, u64)]) -> UnifiedPlan {
        let mut statements = compile(sql).expect("SQL compiles");
        let rows = rows.iter().copied().collect::<BTreeMap<_, _>>();
        optimize_with_statistics(
            UnifiedPlan::lower(statements.remove(0)),
            &OptimizerConfig::default(),
            &|table: &str| {
                rows.get(table).copied().map(|row_count| {
                    let column = || crate::ColumnStats {
                        distinct_count: row_count,
                        row_count,
                        ..crate::ColumnStats::default()
                    };
                    crate::RelationStats::new(row_count)
                        .with_column("id", column())
                        .with_column("a_id", column())
                        .with_column("b_id", column())
                })
            },
        )
        .expect("optimizer succeeds")
    }

    fn query_block(plan: &UnifiedPlan) -> &QueryBlockPlan {
        let UnifiedPlan::Query(query) = plan else {
            panic!("query plan expected");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("query block expected");
        };
        block
    }

    fn source_aliases(source: &SourcePlan) -> BTreeSet<String> {
        match source {
            SourcePlan::Table { name, alias } => {
                BTreeSet::from([alias.clone().unwrap_or_else(|| name.clone())])
            }
            SourcePlan::Join { left, right, .. } => {
                let mut aliases = source_aliases(left);
                aliases.extend(source_aliases(right));
                aliases
            }
            SourcePlan::Values { .. }
            | SourcePlan::Function { .. }
            | SourcePlan::Subquery { .. } => BTreeSet::new(),
        }
    }

    fn conjunct_count(expression: &ScalarExpr) -> usize {
        match expression {
            ScalarExpr::And(items) => items.iter().map(conjunct_count).sum(),
            _ => 1,
        }
    }

    fn source_predicate_count(source: &SourcePlan) -> usize {
        match source {
            SourcePlan::Join {
                left, right, on, ..
            } => {
                source_predicate_count(left)
                    + source_predicate_count(right)
                    + on.as_ref().map_or(0, conjunct_count)
            }
            SourcePlan::Table { .. }
            | SourcePlan::Values { .. }
            | SourcePlan::Function { .. }
            | SourcePlan::Subquery { .. } => 0,
        }
    }

    #[test]
    fn simplifies_boolean_expressions_after_lowering() {
        let UnifiedPlan::Query(query) = optimized("SELECT x FROM t WHERE true AND x = 1") else {
            panic!("query plan expected");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("query block expected");
        };
        assert!(matches!(
            block.r#where,
            Some(ScalarExpr::Binary {
                op: BinaryOp::Equal,
                ..
            })
        ));
    }

    #[test]
    fn selects_operator_tree_access_and_pushes_relational_limit() {
        let UnifiedPlan::Query(query) = optimized(
            "SELECT id FROM docs WHERE text_match(body, 'rust') \
             ORDER BY _score DESC LIMIT 5",
        ) else {
            panic!("query plan expected");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("query block expected");
        };
        assert!(matches!(
            block.access,
            AccessPathPlan::OperatorTree {
                score_limit_pushdown: true
            }
        ));
    }

    #[test]
    fn optimizes_mutation_and_cte_children() {
        let UnifiedPlan::Command(command) = optimized(
            "WITH q AS (SELECT 1 AS x WHERE true) \
             UPDATE t SET x = 1 + 2 WHERE true AND id = 1",
        ) else {
            panic!("command plan expected");
        };
        let CommandPlan::Update(update) = command.as_ref() else {
            panic!("update plan expected");
        };
        assert_eq!(update.ctes.len(), 1);
        assert!(matches!(
            update.predicate,
            Some(ScalarExpr::Binary {
                op: BinaryOp::Equal,
                ..
            })
        ));
    }

    #[test]
    fn optimizes_query_bodies_owned_by_commands() {
        let UnifiedPlan::Command(command) = optimized(
            "PREPARE search AS SELECT id FROM docs \
             WHERE true AND text_match(body, 'rust') \
             ORDER BY _score DESC LIMIT 3",
        ) else {
            panic!("command plan expected");
        };
        let CommandPlan::Prepare { body, .. } = command.as_ref() else {
            panic!("prepare plan expected");
        };
        let UnifiedPlan::Query(query) = body.as_ref() else {
            panic!("prepared query expected");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("query block expected");
        };
        assert!(matches!(
            block.r#where,
            Some(ScalarExpr::Func { ref name, .. }) if name == "text_match"
        ));
        assert!(matches!(
            block.access,
            AccessPathPlan::OperatorTree {
                score_limit_pushdown: true
            }
        ));
    }

    #[test]
    fn selects_hybrid_access_and_prioritizes_retrieval_candidates() {
        let UnifiedPlan::Query(query) = optimized(
            "SELECT id FROM docs \
             WHERE id + 1 > 2 AND text_match(body, 'rust')",
        ) else {
            panic!("query plan expected");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("query block expected");
        };
        assert!(matches!(block.access, AccessPathPlan::Hybrid));
        let Some(ScalarExpr::And(parts)) = block.r#where.as_ref() else {
            panic!("conjunctive predicate expected");
        };
        assert!(matches!(
            parts.first(),
            Some(ScalarExpr::Func { name, .. }) if name == "text_match"
        ));
    }

    #[test]
    fn dpccp_reorders_inner_join_source_from_relation_statistics() {
        let plan = optimized_with_rows(
            "SELECT a.id FROM a \
             JOIN b ON a.id = b.a_id \
             JOIN c ON b.id = c.b_id",
            &[("a", 1_000_000), ("b", 10_000), ("c", 10)],
        );
        let source = query_block(&plan).from.as_ref().expect("join source");
        let SourcePlan::Join {
            left, right, on, ..
        } = source
        else {
            panic!("top-level join expected");
        };

        let left_aliases = source_aliases(left);
        let right_aliases = source_aliases(right);
        let small_pair = BTreeSet::from(["b".to_string(), "c".to_string()]);
        assert!(
            left_aliases == small_pair || right_aliases == small_pair,
            "unexpected DPccp source: {source:?}"
        );
        assert!(on.is_some(), "a-b predicate must remain on the root join");
        assert_eq!(source_predicate_count(source), 2);
    }

    #[test]
    fn join_reordering_preserves_outer_join_boundary() {
        let plan = optimized_with_rows(
            "SELECT a.id FROM a \
             LEFT JOIN b ON a.id = b.a_id \
             JOIN c ON b.id = c.b_id",
            &[("a", 1_000_000), ("b", 10_000), ("c", 1)],
        );
        let source = query_block(&plan).from.as_ref().expect("join source");
        let SourcePlan::Join {
            left,
            right,
            kind: uqa_sql::ast::JoinKind::Inner,
            lateral: false,
            ..
        } = source
        else {
            panic!("original top-level inner join must remain");
        };
        assert!(matches!(
            left.as_ref(),
            SourcePlan::Join {
                kind: uqa_sql::ast::JoinKind::Left,
                ..
            }
        ));
        assert_eq!(source_aliases(right), BTreeSet::from(["c".to_string()]));
        assert_eq!(source_predicate_count(source), 2);
    }

    #[test]
    fn join_reordering_preserves_lateral_boundary() {
        let mut statements = compile(
            "SELECT a.id FROM a \
             JOIN b ON a.id = b.a_id \
             JOIN c ON b.id = c.b_id",
        )
        .expect("SQL compiles");
        let mut plan = UnifiedPlan::lower(statements.remove(0));
        let UnifiedPlan::Query(query) = &mut plan else {
            panic!("query plan expected");
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("query block expected");
        };
        let SourcePlan::Join { lateral, .. } = block.from.as_mut().expect("join source") else {
            panic!("join expected");
        };
        *lateral = true;

        let rows = BTreeMap::from([("a", 1_000_000), ("b", 10_000), ("c", 1)]);
        let plan = optimize_with_statistics(plan, &OptimizerConfig::default(), &|table: &str| {
            rows.get(table).copied().map(|row_count| {
                let column = || crate::ColumnStats {
                    distinct_count: row_count,
                    row_count,
                    ..crate::ColumnStats::default()
                };
                crate::RelationStats::new(row_count)
                    .with_column("id", column())
                    .with_column("a_id", column())
                    .with_column("b_id", column())
            })
        })
        .expect("optimizer succeeds");
        let source = query_block(&plan).from.as_ref().expect("join source");
        let SourcePlan::Join {
            left,
            right,
            lateral: true,
            strategy: JoinExecutionStrategy::Auto,
            ..
        } = source
        else {
            panic!("lateral root boundary must remain unchanged: {source:?}");
        };
        assert_eq!(
            source_aliases(left),
            BTreeSet::from(["a".to_string(), "b".to_string()])
        );
        assert_eq!(source_aliases(right), BTreeSet::from(["c".to_string()]));
        assert!(matches!(
            left.as_ref(),
            SourcePlan::Join {
                strategy: JoinExecutionStrategy::Hash,
                lateral: false,
                ..
            }
        ));
    }
}
