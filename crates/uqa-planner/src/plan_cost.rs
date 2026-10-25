//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational execution and replanning costs in the shared physical cost units.

use uqa_core::memory::ProductionControl;
use uqa_sql::SQLError;

pub mod nodes;
mod scope;
use nodes::{Capture, PlanCostInput};
use scope::{Definition, Definitions, Namespace, ProducerState};

#[cfg(test)]
mod tests;

type Scope<'p, 's> = Option<Namespace<'p, 's>>;

use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, JoinKind, SetOpKind};
use uqa_sql::ScalarExpr;

use crate::{
    CardinalityEstimator, CommandPlan, ComputePlan, CostEstimator, CtePlan, CtePlanBody,
    JoinExecutionStrategy, OperatorKind, QueryBlockPlan, QueryPlan, RelationStats, RelationalPlan,
    SourcePlan, SourceStatistics, UnifiedPlan,
};

const DEFAULT_ROWS: f64 = 1_000.0;

/// Work required by one executable plan, independent of a particular invocation.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlanCost {
    pub rows: f64,
    pub execution: f64,
    /// Work required before this plan can emit its first row.
    pub startup: f64,
    pub relations: usize,
}

impl PlanCost {
    /// Charge the per-relation planner work in the same units as scalar evaluation.
    pub fn including_planning(self, costs: &CostEstimator) -> f64 {
        self.execution + 1_000.0 * costs.coefficients.filter_per_row * (self.relations + 1) as f64
    }

    fn add_child(&mut self, child: Self) {
        self.execution += child.execution;
        self.relations += child.relations;
    }
}

/// Costs relational operators without evaluating functions or executing query children.
pub struct PlanCostEstimator<'a, 'c> {
    statistics: &'a dyn SourceStatistics,
    costs: CostEstimator,
    cardinality: CardinalityEstimator,
    control: ProductionControl<'c>,
}

impl<'a> PlanCostEstimator<'a, 'static> {
    pub fn new(statistics: &'a dyn SourceStatistics) -> Self {
        Self {
            statistics,
            costs: CostEstimator::default(),
            cardinality: CardinalityEstimator::new(),
            control: ProductionControl::uncontrolled(),
        }
    }

    pub fn estimate(&self, plan: &UnifiedPlan) -> PlanCost {
        self.estimate_with_control(plan, &ProductionControl::uncontrolled())
            .expect("ordinary plan cost estimation")
    }

    pub fn estimate_with_control(
        &self,
        plan: &UnifiedPlan,
        control: &ProductionControl<'_>,
    ) -> Result<PlanCost, SQLError> {
        self.estimate_captured(plan, control, None)
    }

    /// Capture native query/source costs during the same lexical traversal used for the root estimate.
    pub fn estimate_nodes_with_control<'p>(
        &self,
        plan: &'p UnifiedPlan,
        control: &ProductionControl<'_>,
    ) -> Result<nodes::PlanCostTree<'p>, SQLError> {
        control.check()?;
        let capture = Capture::new(*control);
        let cost = self.estimate_captured(plan, control, Some(&capture))?;
        capture.finish(cost)
    }

    fn estimate_captured<'p>(
        &self,
        plan: &'p UnifiedPlan,
        control: &ProductionControl<'_>,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<PlanCost, SQLError> {
        let estimator = PlanCostEstimator {
            statistics: self.statistics,
            costs: self.costs.clone(),
            cardinality: CardinalityEstimator::new(),
            control: *control,
        };
        estimator.control.check()?;
        match plan {
            UnifiedPlan::Query(query) => estimator.query(query, None, capture),
            UnifiedPlan::Command(command) => estimator.command(command, None, capture),
        }
    }
}

impl PlanCostEstimator<'_, '_> {
    fn unary(&self, kind: OperatorKind, rows: f64) -> f64 {
        self.costs.estimate_unary(kind, rows).total()
    }

    fn producer<'p>(
        &self,
        definition: Definition<'p, '_>,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<PlanCost, SQLError> {
        self.control.check()?;
        match definition.state() {
            ProducerState::Estimated(cost) => return Ok(cost),
            ProducerState::Estimating => {
                return Ok(PlanCost {
                    rows: DEFAULT_ROWS,
                    ..PlanCost::default()
                })
            }
            ProducerState::Unestimated => {}
        }
        definition.set_state(ProducerState::Estimating);
        let scope = definition.namespace();
        let cost = Capture::at_parent(capture, definition.parent(), || {
            match &definition.plan().body {
                CtePlanBody::Query(query) => self.query(query, scope, capture),
                CtePlanBody::Command(command) => self.command(command, scope, capture),
            }
        })?;
        definition.set_state(ProducerState::Estimated(cost));
        Ok(cost)
    }

    fn finish_definitions<'p>(
        &self,
        definitions: &Definitions<'p, '_>,
        order: &[usize],
        cost: &mut PlanCost,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<(), SQLError> {
        for index in order {
            self.control.check()?;
            let definition = definitions.definition(*index);
            if definition.plan().body.modifies_data() {
                self.producer(definition, capture)?;
            }
        }
        for state in definitions.states() {
            self.control.check()?;
            if let ProducerState::Estimated(producer) = state {
                cost.startup += producer.startup;
                cost.add_child(producer);
            }
        }
        Ok(())
    }

    fn query<'p>(
        &self,
        query: &'p QueryPlan,
        outer: Scope<'p, '_>,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<PlanCost, SQLError> {
        Capture::node(capture, PlanCostInput::Query(query), || {
            self.query_cost(query, outer, capture)
        })
    }

    fn query_cost<'p>(
        &self,
        query: &'p QueryPlan,
        outer: Scope<'p, '_>,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<PlanCost, SQLError> {
        self.control.check()?;
        let order =
            uqa_sql::semantics::reachable_query_cte_order_with_control(query, self.control)?;
        let definitions =
            Definitions::new(&query.ctes, outer, Capture::parent(capture), self.control)?;
        let scope = definitions.namespace();
        let mut cost = match &query.root {
            RelationalPlan::QueryBlock(block) => self.block(block, scope, capture)?,
            RelationalPlan::Values { rows, subqueries } => {
                let mut cost = self.values(rows);
                self.subqueries(&mut cost, subqueries, scope, capture)?;
                cost
            }
            RelationalPlan::SetOp {
                kind,
                all,
                left,
                right,
                order_by,
                limit,
                offset,
                subqueries,
                ..
            } => {
                let mut left = self.query(left, scope, capture)?;
                let right = self.query(right, scope, capture)?;
                let input_rows = left.rows + right.rows;
                left.add_child(right);
                left.rows = match kind {
                    SetOpKind::Union => input_rows,
                    SetOpKind::Intersect => left.rows.min(right.rows),
                    SetOpKind::Except => left.rows,
                };
                if !all || !matches!(kind, SetOpKind::Union) {
                    left.execution += self.unary(OperatorKind::HashAggregate, input_rows);
                    left.startup = left.execution;
                }
                self.subqueries(&mut left, subqueries, scope, capture)?;
                self.finish(
                    &mut left,
                    !order_by.is_empty(),
                    limit.as_deref(),
                    offset.as_deref(),
                );
                left
            }
        };
        self.finish_definitions(&definitions, &order, &mut cost, capture)?;
        Ok(cost)
    }

    fn block<'p>(
        &self,
        block: &'p QueryBlockPlan,
        scope: Scope<'p, '_>,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<PlanCost, SQLError> {
        self.control.check()?;
        let mut cost = match &block.from {
            Some(source) => self.source(source, block.r#where.as_ref(), scope, capture)?,
            None => {
                let mut cost = PlanCost {
                    rows: 1.0,
                    ..PlanCost::default()
                };
                self.filter(&mut cost, block.r#where.as_ref(), &RelationStats::default());
                cost
            }
        };
        self.subqueries(&mut cost, &block.subqueries, scope, capture)?;
        match block.compute {
            ComputePlan::Aggregate => {
                cost.execution += self.unary(OperatorKind::HashAggregate, cost.rows);
                cost.startup = cost.execution;
                cost.rows = if block.group_by.is_empty() && block.grouping_sets.is_empty() {
                    1.0
                } else {
                    cost.rows.min(200.0) * block.grouping_sets.len().max(1) as f64
                };
                self.filter(&mut cost, block.having.as_ref(), &RelationStats::default());
            }
            ComputePlan::Window => {
                cost.execution += self.unary(OperatorKind::Window, cost.rows);
                cost.startup = cost.execution;
            }
            ComputePlan::Project => {}
        }
        cost.execution +=
            self.unary(OperatorKind::Project, cost.rows) * block.projections.len() as f64;
        if block.distinct || !block.distinct_on.is_empty() {
            cost.execution += self.unary(OperatorKind::HashAggregate, cost.rows);
            cost.startup = cost.execution;
        }
        self.finish(
            &mut cost,
            !block.order_by.is_empty(),
            block.limit.as_ref(),
            block.offset.as_ref(),
        );
        Ok(cost)
    }

    fn table(&self, table: &str, predicate: Option<&ScalarExpr>) -> Result<PlanCost, SQLError> {
        self.control.check()?;
        let stats = self
            .statistics
            .relation_statistics(table)
            .unwrap_or_else(|| RelationStats {
                row_count: DEFAULT_ROWS as u64,
                ..RelationStats::default()
            });
        let mut cost = PlanCost {
            rows: stats.row_count as f64,
            execution: self.unary(OperatorKind::TableScan, stats.row_count as f64),
            relations: 1,
            startup: 0.0,
        };
        if let Some(predicate) = predicate {
            if literal_false(predicate) {
                cost.rows = 0.0;
                cost.execution = 0.0;
            } else if let Some(access) = self.statistics.local_access_estimate(table, predicate) {
                cost.rows = access.output_rows;
                cost.execution = access.cost;
                cost.startup = access.cost;
            } else {
                self.filter(&mut cost, Some(predicate), &stats);
            }
        }
        self.control.check()?;
        Ok(cost)
    }

    fn source<'p>(
        &self,
        source: &'p SourcePlan,
        predicate: Option<&ScalarExpr>,
        scope: Scope<'p, '_>,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<PlanCost, SQLError> {
        Capture::node(capture, PlanCostInput::Source(source), || {
            self.source_cost(source, predicate, scope, capture)
        })
    }

    fn source_cost<'p>(
        &self,
        source: &'p SourcePlan,
        predicate: Option<&ScalarExpr>,
        scope: Scope<'p, '_>,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<PlanCost, SQLError> {
        self.control.check()?;
        let mut cost = match source {
            SourcePlan::Table {
                name,
                executable_query,
                ..
            } => {
                let cte = scope.and_then(|scope| scope.resolve(name));
                if let Some(query) = executable_query {
                    // Retained folded CTEs use their definition namespace; stored views have an independent namespace.
                    self.query(query, cte.and_then(Definition::namespace), capture)?
                } else if let Some(cte) = cte {
                    let producer = self.producer(cte, capture)?;
                    PlanCost {
                        rows: producer.rows,
                        execution: self.unary(OperatorKind::Project, producer.rows),
                        ..PlanCost::default()
                    }
                } else {
                    return self.table(name, predicate);
                }
            }
            SourcePlan::Values { rows, .. } => self.values(rows),
            SourcePlan::RetainedRows(source) => PlanCost {
                rows: source.rows() as f64,
                execution: self.unary(OperatorKind::TableScan, source.rows() as f64),
                relations: 0,
                startup: 0.0,
            },
            SourcePlan::Subquery { body, .. } => self.query(body, scope, capture)?,
            SourcePlan::Join {
                left,
                right,
                kind,
                on,
                using,
                natural,
                strategy,
                lateral,
                ..
            } => {
                let left = self.source(left, None, scope, capture)?;
                let right = self.source(right, None, scope, capture)?;
                let equality = on.as_ref().is_some_and(has_equality) || using.is_some() || *natural;
                let operator = if matches!(kind, JoinKind::Cross) {
                    OperatorKind::CrossJoin
                } else if equality || matches!(strategy, JoinExecutionStrategy::Hash) {
                    if matches!(kind, JoinKind::Inner) {
                        OperatorKind::HashJoinInner
                    } else {
                        OperatorKind::HashJoinOuter
                    }
                } else {
                    OperatorKind::NestedLoopJoin
                };
                let selectivity = on.as_ref().map_or_else(
                    || {
                        if using.is_some() || *natural {
                            self.cardinality.default_selectivity
                        } else {
                            1.0
                        }
                    },
                    |predicate| {
                        self.cardinality
                            .scalar_selectivity(predicate, &RelationStats::default())
                            .raw()
                    },
                );
                self.join_cost(*kind, operator, *lateral, left, right, selectivity)
            }
            SourcePlan::Function { .. } => {
                self.statistics.source_access_estimate(source).map_or_else(
                    || PlanCost {
                        rows: DEFAULT_ROWS,
                        execution: self.unary(OperatorKind::Project, DEFAULT_ROWS),
                        relations: 1,
                        startup: 0.0,
                    },
                    |access| PlanCost {
                        rows: access.output_rows,
                        execution: access.cost,
                        startup: access.cost,
                        relations: 1,
                    },
                )
            }
            SourcePlan::FunctionGroup { functions, .. } => PlanCost {
                rows: DEFAULT_ROWS,
                execution: functions.len() as f64 * self.unary(OperatorKind::Project, DEFAULT_ROWS),
                relations: functions.len(),
                startup: 0.0,
            },
        };
        self.filter(&mut cost, predicate, &RelationStats::default());
        self.control.check()?;
        Ok(cost)
    }

    fn join_cost(
        &self,
        kind: JoinKind,
        operator: OperatorKind,
        lateral: bool,
        mut left: PlanCost,
        right: PlanCost,
        selectivity: f64,
    ) -> PlanCost {
        left.startup = if matches!(
            operator,
            OperatorKind::HashJoinInner | OperatorKind::HashJoinOuter
        ) {
            let (build, probe) =
                if matches!(operator, OperatorKind::HashJoinInner) && left.rows < right.rows {
                    (left, right)
                } else {
                    (right, left)
                };
            build.execution
                + probe.startup
                + self.costs.estimate_hash_build(operator, build.rows).total()
        } else {
            left.startup + right.startup
        };
        left.execution += self
            .costs
            .estimate_join(operator, left.rows, right.rows)
            .total();
        if lateral {
            left.execution += right.execution * (left.rows - 1.0).max(0.0);
        }
        left.add_child(right);
        let rows = left.rows * right.rows * selectivity;
        left.rows = match kind {
            JoinKind::Left => rows.max(left.rows),
            JoinKind::Right => rows.max(right.rows),
            JoinKind::Full => rows.max(left.rows).max(right.rows),
            JoinKind::Inner | JoinKind::Cross => rows,
        };
        left
    }

    fn values(&self, rows: &[Vec<ScalarExpr>]) -> PlanCost {
        PlanCost {
            rows: rows.len() as f64,
            execution: self.unary(
                OperatorKind::Project,
                rows.iter().map(Vec::len).sum::<usize>() as f64,
            ),
            relations: 0,
            startup: 0.0,
        }
    }

    fn filter(&self, cost: &mut PlanCost, predicate: Option<&ScalarExpr>, stats: &RelationStats) {
        if let Some(predicate) = predicate {
            cost.execution += self.unary(OperatorKind::Filter, cost.rows);
            cost.rows *= self.cardinality.scalar_selectivity(predicate, stats).raw();
        }
    }

    fn subqueries<'p>(
        &self,
        cost: &mut PlanCost,
        queries: &'p [QueryPlan],
        scope: Scope<'p, '_>,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<(), SQLError> {
        for query in queries {
            let mut child = self.query(query, scope, capture)?;
            cost.startup += child.execution;
            child.execution *= cost.rows.max(1.0);
            cost.add_child(child);
        }
        Ok(())
    }

    fn finish(
        &self,
        cost: &mut PlanCost,
        sort: bool,
        limit: Option<&ScalarExpr>,
        offset: Option<&ScalarExpr>,
    ) {
        if sort {
            cost.execution += self.unary(OperatorKind::Sort, cost.rows);
            cost.startup = cost.execution;
        }
        let input_rows = cost.rows;
        let run = (cost.execution - cost.startup).max(0.0);
        if let Some(offset) = offset.and_then(literal_count) {
            let skipped = cost.rows.min(offset);
            if input_rows > 0.0 {
                cost.startup += run * skipped / input_rows;
            }
            cost.rows -= skipped;
        }
        if let Some(limit) = limit.and_then(literal_count) {
            cost.rows = cost.rows.min(limit);
            if input_rows > 0.0 {
                cost.execution = cost.startup + run * cost.rows / input_rows;
            }
        }
        if limit.is_some() || offset.is_some() {
            cost.execution += self.unary(OperatorKind::Limit, cost.rows);
        }
    }

    fn command<'p>(
        &self,
        command: &'p CommandPlan,
        outer: Scope<'p, '_>,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<PlanCost, SQLError> {
        Capture::node(capture, PlanCostInput::Command(command), || {
            self.command_cost(command, outer, capture)
        })
    }

    fn command_cost<'p>(
        &self,
        command: &'p CommandPlan,
        outer: Scope<'p, '_>,
        capture: Option<&Capture<'p, '_>>,
    ) -> Result<PlanCost, SQLError> {
        self.control.check()?;
        let order =
            uqa_sql::semantics::reachable_command_cte_order_with_control(command, self.control)?;
        let definitions = Definitions::new(
            command.ctes(),
            outer,
            Capture::parent(capture),
            self.control,
        )?;
        let scope = definitions.namespace();
        let mut cost = match command {
            CommandPlan::Insert(plan) => plan.source.as_ref().map_or_else(
                || Ok(self.values(&plan.rows)),
                |query| self.query(query, scope, capture),
            )?,
            CommandPlan::Update(plan) => self.table(&plan.table, plan.predicate.as_ref())?,
            CommandPlan::Delete(plan) => self.table(&plan.table, plan.predicate.as_ref())?,
            CommandPlan::Merge(plan) => self.table(&plan.target, plan.target_predicate.as_ref())?,
            CommandPlan::Explain { body, .. } => match body.as_ref() {
                UnifiedPlan::Query(query) => self.query(query, scope, capture)?,
                UnifiedPlan::Command(command) => self.command(command, scope, capture)?,
            },
            _ => PlanCost::default(),
        };
        if let Some(source) = command.source_input() {
            let source = self.source(source, None, scope, capture)?;
            cost.startup += source.startup;
            cost.execution += self
                .costs
                .estimate_join(OperatorKind::NestedLoopJoin, cost.rows, source.rows)
                .total();
            cost.add_child(source);
        }
        self.subqueries(&mut cost, command.scalar_subqueries(), scope, capture)?;
        if command.mutation_target().is_some() {
            cost.execution += self.unary(OperatorKind::TableScan, cost.rows);
        }
        self.finish_definitions(&definitions, &order, &mut cost, capture)?;
        Ok(cost)
    }
}

fn literal_count(expression: &ScalarExpr) -> Option<f64> {
    match expression {
        ScalarExpr::Literal(Value::Int(value))
        | ScalarExpr::TypedLiteral {
            value: Value::Int(value),
            ..
        } if *value >= 0 => Some(*value as f64),
        _ => None,
    }
}

fn literal_false(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Literal(Value::Bool(false) | Value::Null)
            | ScalarExpr::TypedLiteral {
                value: Value::Bool(false) | Value::Null,
                ..
            }
    )
}

fn has_equality(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Binary {
            op: BinaryOp::Equal,
            ..
        } => true,
        ScalarExpr::And(items) => items.iter().any(has_equality),
        _ => false,
    }
}
