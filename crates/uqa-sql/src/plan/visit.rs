//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only traversal of every scalar expression a plan owns: the counterpart of the plan rewrite for analyses that check a plan without changing it.

use super::{
    CommandPlan, CtePlan, CtePlanBody, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};
use crate::ir::ScalarExpr;

impl UnifiedPlan {
    /// Visit every scalar expression this plan owns, each root once, including those of its CTEs, relational sources, scalar subqueries and command inputs. The nested nodes of a root are reached through [`ScalarExpr::visit`].
    pub fn visit_scalar_expressions(&self, visit: &mut dyn FnMut(&ScalarExpr)) {
        match self {
            Self::Query(query) => query.visit_scalar_expressions(visit),
            Self::Command(command) => command.visit_scalar_expressions(visit),
        }
    }
}

fn visit_ctes(ctes: &[CtePlan], visit: &mut dyn FnMut(&ScalarExpr)) {
    for cte in ctes {
        match &cte.body {
            CtePlanBody::Query(query) => query.visit_scalar_expressions(visit),
            CtePlanBody::Command(command) => command.visit_scalar_expressions(visit),
        }
    }
}

impl QueryPlan {
    /// Visit every scalar expression this query owns, each root once, including those of its CTEs, relational sources and scalar subqueries.
    pub fn visit_scalar_expressions(&self, visit: &mut dyn FnMut(&ScalarExpr)) {
        visit_ctes(&self.ctes, visit);
        match &self.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &block.from {
                    source.visit_scalar_expressions(visit);
                }
                for expression in block
                    .r#where
                    .iter()
                    .chain(block.projections.iter().map(|projection| &projection.expr))
                    .chain(&block.group_by)
                    .chain(block.grouping_sets.iter().flatten())
                    .chain(block.having.iter())
                    .chain(block.order_by.iter().map(|order| &order.expr))
                    .chain(block.limit.iter())
                    .chain(block.offset.iter())
                    .chain(&block.distinct_on)
                {
                    visit(expression);
                }
                for subquery in &block.subqueries {
                    subquery.visit_scalar_expressions(visit);
                }
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
                left.visit_scalar_expressions(visit);
                right.visit_scalar_expressions(visit);
                for expression in order_by
                    .iter()
                    .map(|order| &order.expr)
                    .chain(limit.as_deref())
                    .chain(offset.as_deref())
                {
                    visit(expression);
                }
                for subquery in subqueries {
                    subquery.visit_scalar_expressions(visit);
                }
            }
            RelationalPlan::Values { rows, subqueries } => {
                for expression in rows.iter().flatten() {
                    visit(expression);
                }
                for subquery in subqueries {
                    subquery.visit_scalar_expressions(visit);
                }
            }
        }
    }
}

impl SourcePlan {
    /// Visit every scalar expression this source owns, each root once: join conditions, `VALUES` rows, function arguments and the expressions of the query bodies nested in derived tables.
    pub fn visit_scalar_expressions(&self, visit: &mut dyn FnMut(&ScalarExpr)) {
        match self {
            Self::Table { .. } => {}
            Self::Subquery { body, .. } => body.visit_scalar_expressions(visit),
            Self::Join {
                left, right, on, ..
            } => {
                left.visit_scalar_expressions(visit);
                right.visit_scalar_expressions(visit);
                if let Some(on) = on {
                    visit(on);
                }
            }
            Self::Values { rows, .. } => {
                for expression in rows.iter().flatten() {
                    visit(expression);
                }
            }
            Self::Function { args, .. } => {
                for argument in args {
                    visit(argument);
                }
            }
            Self::FunctionGroup { functions, .. } => {
                for argument in functions.iter().flat_map(|function| &function.args) {
                    visit(argument);
                }
            }
        }
    }
}

impl CommandPlan {
    /// Visit every scalar expression this command owns, each root once, including those of its CTEs, query inputs and source.
    pub fn visit_scalar_expressions(&self, visit: &mut dyn FnMut(&ScalarExpr)) {
        visit_ctes(self.ctes(), visit);
        for expression in self.expressions() {
            visit(expression);
        }
        for query in self.query_inputs() {
            query.visit_scalar_expressions(visit);
        }
        if let Some(source) = self.source_input() {
            source.visit_scalar_expressions(visit);
        }
    }
}
