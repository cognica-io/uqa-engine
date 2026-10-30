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
        let Ok(()) = self.try_visit_scalar_expressions(&mut |expression| {
            visit(expression);
            Ok::<_, std::convert::Infallible>(())
        });
    }

    /// Stop at the first callback error, without copying syntax or visiting later inputs.
    pub fn try_visit_scalar_expressions<'a, E>(
        &'a self,
        visit: &mut dyn FnMut(&'a ScalarExpr) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::Query(query) => query.try_visit_scalar_expressions(visit)?,
            Self::Command(command) => command.try_visit_scalar_expressions(visit)?,
        }
        Ok(())
    }
}

impl<'a> super::PlanRef<'a> {
    pub fn visit_scalar_expressions(self, visit: &mut dyn FnMut(&ScalarExpr)) {
        let Ok(()) = self.try_visit_scalar_expressions(&mut |expression| {
            visit(expression);
            Ok::<_, std::convert::Infallible>(())
        });
    }

    /// Stop at the first callback error, without copying syntax or visiting later inputs.
    pub fn try_visit_scalar_expressions<E>(
        self,
        visit: &mut dyn FnMut(&'a ScalarExpr) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::Query(query) => query.try_visit_scalar_expressions(visit)?,
            Self::Command(command) => command.try_visit_scalar_expressions(visit)?,
        }
        Ok(())
    }
}

fn try_visit_ctes<'a, E>(
    ctes: &'a [CtePlan],
    visit: &mut dyn FnMut(&'a ScalarExpr) -> Result<(), E>,
) -> Result<(), E> {
    for cte in ctes {
        match &cte.body {
            CtePlanBody::Query(query) => query.try_visit_scalar_expressions(visit)?,
            CtePlanBody::Command(command) => command.try_visit_scalar_expressions(visit)?,
        }
        if let Some(cycle) = &cte.cycle {
            visit(&cycle.mark_value)?;
            visit(&cycle.mark_default)?;
        }
    }
    Ok(())
}

impl QueryPlan {
    /// Visit every scalar expression this query owns, each root once, including those of its CTEs, relational sources and scalar subqueries.
    pub fn visit_scalar_expressions(&self, visit: &mut dyn FnMut(&ScalarExpr)) {
        let Ok(()) = self.try_visit_scalar_expressions(&mut |expression| {
            visit(expression);
            Ok::<_, std::convert::Infallible>(())
        });
    }

    /// Stop at the first callback error, without copying syntax or visiting later inputs.
    pub fn try_visit_scalar_expressions<'a, E>(
        &'a self,
        visit: &mut dyn FnMut(&'a ScalarExpr) -> Result<(), E>,
    ) -> Result<(), E> {
        try_visit_ctes(&self.ctes, visit)?;
        match &self.root {
            RelationalPlan::QueryBlock(block) => {
                for rewrite in &block.deferred_outer_rewrites {
                    rewrite.try_visit_scalar_expressions(visit)?;
                }
                if let Some(source) = &block.from {
                    source.try_visit_scalar_expressions(visit)?;
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
                    visit(expression)?;
                }
                for window in &block.windows {
                    for expression in window.spec.expressions() {
                        visit(expression)?;
                    }
                }
                for subquery in &block.subqueries {
                    subquery.try_visit_scalar_expressions(visit)?;
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
                left.try_visit_scalar_expressions(visit)?;
                right.try_visit_scalar_expressions(visit)?;
                for expression in order_by
                    .iter()
                    .map(|order| &order.expr)
                    .chain(limit.as_deref())
                    .chain(offset.as_deref())
                {
                    visit(expression)?;
                }
                for subquery in subqueries {
                    subquery.try_visit_scalar_expressions(visit)?;
                }
            }
            RelationalPlan::Values { rows, subqueries } => {
                for expression in rows.iter().flatten() {
                    visit(expression)?;
                }
                for subquery in subqueries {
                    subquery.try_visit_scalar_expressions(visit)?;
                }
            }
        }
        Ok(())
    }
}

impl SourcePlan {
    /// Visit every scalar expression this source owns, each root once: join conditions, `VALUES` rows, function arguments and the expressions of the query bodies nested in derived tables.
    pub fn visit_scalar_expressions(&self, visit: &mut dyn FnMut(&ScalarExpr)) {
        let Ok(()) = self.try_visit_scalar_expressions(&mut |expression| {
            visit(expression);
            Ok::<_, std::convert::Infallible>(())
        });
    }

    /// Stop at the first callback error, without copying syntax or visiting later inputs.
    pub fn try_visit_scalar_expressions<'a, E>(
        &'a self,
        visit: &mut dyn FnMut(&'a ScalarExpr) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            crate::plan::SourcePlan::RetainedRows(_) => {}
            Self::Table { .. } => {}
            Self::Subquery { body, .. } => body.try_visit_scalar_expressions(visit)?,
            Self::Join {
                left, right, on, ..
            } => {
                left.try_visit_scalar_expressions(visit)?;
                right.try_visit_scalar_expressions(visit)?;
                if let Some(on) = on {
                    visit(on)?;
                }
            }
            Self::Values { rows, .. } => {
                for expression in rows.iter().flatten() {
                    visit(expression)?;
                }
            }
            Self::Function { args, .. } => {
                for argument in args {
                    visit(argument)?;
                }
            }
            Self::FunctionGroup { functions, .. } => {
                for argument in functions.iter().flat_map(|function| &function.args) {
                    visit(argument)?;
                }
            }
        }
        Ok(())
    }
}

mod command;
