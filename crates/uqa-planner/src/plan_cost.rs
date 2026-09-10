//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational execution and replanning costs in the shared physical cost units.

use std::collections::BTreeMap;

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
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanCost {
    pub rows: f64,
    pub execution: f64,
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
pub struct PlanCostEstimator<'a> {
    statistics: &'a dyn SourceStatistics,
    costs: CostEstimator,
    cardinality: CardinalityEstimator,
}

impl<'a> PlanCostEstimator<'a> {
    pub fn new(statistics: &'a dyn SourceStatistics) -> Self {
        Self {
            statistics,
            costs: CostEstimator::default(),
            cardinality: CardinalityEstimator::new(),
        }
    }

    pub fn estimate(&self, plan: &UnifiedPlan) -> PlanCost {
        match plan {
            UnifiedPlan::Query(query) => self.query(query, &BTreeMap::new()),
            UnifiedPlan::Command(command) => self.command(command, &BTreeMap::new()),
        }
    }

    fn unary(&self, kind: OperatorKind, rows: f64) -> f64 {
        self.costs.estimate_unary(kind, rows).total()
    }

    fn ctes(&self, ctes: &[CtePlan], scope: &mut BTreeMap<String, PlanCost>) -> PlanCost {
        let mut total = PlanCost::default();
        for cte in ctes {
            if cte.recursive {
                scope.insert(
                    cte.name.clone(),
                    PlanCost {
                        rows: DEFAULT_ROWS,
                        ..PlanCost::default()
                    },
                );
            }
            let cost = match &cte.body {
                CtePlanBody::Query(query) => self.query(query, scope),
                CtePlanBody::Command(command) => self.command(command, scope),
            };
            total.add_child(cost);
            scope.insert(
                cte.name.clone(),
                PlanCost {
                    rows: cost.rows,
                    ..PlanCost::default()
                },
            );
        }
        total
    }

    fn query(&self, query: &QueryPlan, outer: &BTreeMap<String, PlanCost>) -> PlanCost {
        let mut scope = outer.clone();
        let ctes = self.ctes(&query.ctes, &mut scope);
        let mut cost = match &query.root {
            RelationalPlan::QueryBlock(block) => self.block(block, &scope),
            RelationalPlan::Values { rows, subqueries } => {
                let mut cost = self.values(rows);
                self.subqueries(&mut cost, subqueries, &scope);
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
                let mut left = self.query(left, &scope);
                let right = self.query(right, &scope);
                let input_rows = left.rows + right.rows;
                left.add_child(right);
                left.rows = match kind {
                    SetOpKind::Union => input_rows,
                    SetOpKind::Intersect => left.rows.min(right.rows),
                    SetOpKind::Except => left.rows,
                };
                if !all || !matches!(kind, SetOpKind::Union) {
                    left.execution += self.unary(OperatorKind::HashAggregate, input_rows);
                }
                self.subqueries(&mut left, subqueries, &scope);
                self.finish(
                    &mut left,
                    !order_by.is_empty(),
                    limit.as_deref(),
                    offset.as_deref(),
                );
                left
            }
        };
        cost.add_child(ctes);
        cost
    }

    fn block(&self, block: &QueryBlockPlan, scope: &BTreeMap<String, PlanCost>) -> PlanCost {
        let mut cost = match &block.from {
            Some(source) => self.source(source, block.r#where.as_ref(), scope),
            None => {
                let mut cost = PlanCost {
                    rows: 1.0,
                    ..PlanCost::default()
                };
                self.filter(&mut cost, block.r#where.as_ref(), &RelationStats::default());
                cost
            }
        };
        self.subqueries(&mut cost, &block.subqueries, scope);
        match block.compute {
            ComputePlan::Aggregate => {
                cost.execution += self.unary(OperatorKind::HashAggregate, cost.rows);
                cost.rows = if block.group_by.is_empty() && block.grouping_sets.is_empty() {
                    1.0
                } else {
                    cost.rows.min(200.0) * block.grouping_sets.len().max(1) as f64
                };
                self.filter(&mut cost, block.having.as_ref(), &RelationStats::default());
            }
            ComputePlan::Window => cost.execution += self.unary(OperatorKind::Window, cost.rows),
            ComputePlan::Project => {}
        }
        cost.execution +=
            self.unary(OperatorKind::Project, cost.rows) * block.projections.len() as f64;
        if block.distinct || !block.distinct_on.is_empty() {
            cost.execution += self.unary(OperatorKind::HashAggregate, cost.rows);
        }
        self.finish(
            &mut cost,
            !block.order_by.is_empty(),
            block.limit.as_ref(),
            block.offset.as_ref(),
        );
        cost
    }

    fn table(&self, table: &str, predicate: Option<&ScalarExpr>) -> PlanCost {
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
        };
        if let Some(predicate) = predicate {
            if literal_false(predicate) {
                cost.rows = 0.0;
                cost.execution = 0.0;
            } else if let Some(access) = self.statistics.local_access_estimate(table, predicate) {
                cost.rows = access.output_rows;
                cost.execution = access.cost;
            } else {
                self.filter(&mut cost, Some(predicate), &stats);
            }
        }
        cost
    }

    fn source(
        &self,
        source: &SourcePlan,
        predicate: Option<&ScalarExpr>,
        scope: &BTreeMap<String, PlanCost>,
    ) -> PlanCost {
        if let SourcePlan::Table { name, .. } = source {
            if !scope.contains_key(name) {
                return self.table(name, predicate);
            }
        }
        let mut cost = match source {
            SourcePlan::Table { name, .. } => {
                let mut cost = scope[name];
                cost.execution += self.unary(OperatorKind::Project, cost.rows);
                cost
            }
            SourcePlan::Values { rows, .. } => self.values(rows),
            SourcePlan::Subquery { body, .. } => self.query(body, scope),
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
                let mut left = self.source(left, None, scope);
                let right = self.source(right, None, scope);
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
                left.execution += self
                    .costs
                    .estimate_join(operator, left.rows, right.rows)
                    .total();
                if *lateral {
                    left.execution += right.execution * (left.rows - 1.0).max(0.0);
                }
                left.add_child(right);
                let rows = left.rows
                    * right.rows
                    * if matches!(kind, JoinKind::Cross) {
                        1.0
                    } else {
                        self.cardinality.default_selectivity
                    };
                left.rows = match kind {
                    JoinKind::Left => rows.max(left.rows),
                    JoinKind::Right => rows.max(right.rows),
                    JoinKind::Full => rows.max(left.rows).max(right.rows),
                    JoinKind::Inner | JoinKind::Cross => rows,
                };
                left
            }
            SourcePlan::Function { .. } => {
                self.statistics.source_access_estimate(source).map_or_else(
                    || PlanCost {
                        rows: DEFAULT_ROWS,
                        execution: self.unary(OperatorKind::Project, DEFAULT_ROWS),
                        relations: 1,
                    },
                    |access| PlanCost {
                        rows: access.output_rows,
                        execution: access.cost,
                        relations: 1,
                    },
                )
            }
            SourcePlan::FunctionGroup { functions, .. } => PlanCost {
                rows: DEFAULT_ROWS,
                execution: functions.len() as f64 * self.unary(OperatorKind::Project, DEFAULT_ROWS),
                relations: functions.len(),
            },
        };
        self.filter(&mut cost, predicate, &RelationStats::default());
        cost
    }

    fn values(&self, rows: &[Vec<ScalarExpr>]) -> PlanCost {
        PlanCost {
            rows: rows.len() as f64,
            execution: self.unary(
                OperatorKind::Project,
                rows.iter().map(Vec::len).sum::<usize>() as f64,
            ),
            relations: 0,
        }
    }

    fn filter(&self, cost: &mut PlanCost, predicate: Option<&ScalarExpr>, stats: &RelationStats) {
        if let Some(predicate) = predicate {
            cost.execution += self.unary(OperatorKind::Filter, cost.rows);
            cost.rows *= self.cardinality.scalar_selectivity(predicate, stats).raw();
        }
    }

    fn subqueries(
        &self,
        cost: &mut PlanCost,
        queries: &[QueryPlan],
        scope: &BTreeMap<String, PlanCost>,
    ) {
        for query in queries {
            let mut child = self.query(query, scope);
            child.execution *= cost.rows.max(1.0);
            cost.add_child(child);
        }
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
        }
        if let Some(offset) = offset.and_then(literal_count) {
            cost.rows = (cost.rows - offset).max(0.0);
        }
        if let Some(limit) = limit.and_then(literal_count) {
            cost.rows = cost.rows.min(limit);
        }
        if limit.is_some() || offset.is_some() {
            cost.execution += self.unary(OperatorKind::Limit, cost.rows);
        }
    }

    fn command(&self, command: &CommandPlan, outer: &BTreeMap<String, PlanCost>) -> PlanCost {
        let mut scope = outer.clone();
        let ctes = self.ctes(command.ctes(), &mut scope);
        let mut cost = match command {
            CommandPlan::Insert(plan) => plan.source.as_ref().map_or_else(
                || self.values(&plan.rows),
                |query| self.query(query, &scope),
            ),
            CommandPlan::Update(plan) => self.table(&plan.table, plan.predicate.as_ref()),
            CommandPlan::Delete(plan) => self.table(&plan.table, plan.predicate.as_ref()),
            CommandPlan::Merge(plan) => self.table(&plan.target, plan.target_predicate.as_ref()),
            CommandPlan::Explain { body, .. } => self.estimate(body),
            _ => PlanCost::default(),
        };
        if let Some(source) = command.source_input() {
            let source = self.source(source, None, &scope);
            cost.execution += self
                .costs
                .estimate_join(OperatorKind::NestedLoopJoin, cost.rows, source.rows)
                .total();
            cost.add_child(source);
        }
        self.subqueries(&mut cost, command.scalar_subqueries(), &scope);
        if command.mutation_target().is_some() {
            cost.execution += self.unary(OperatorKind::TableScan, cost.rows);
        }
        cost.add_child(ctes);
        cost
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
