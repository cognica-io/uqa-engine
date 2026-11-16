//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};
use crate::{
    ast::{Expr, SelectStmt},
    plan::{retirement::Pending, AggregateClassifier, QueryBlockPlan, QueryPlan, RelationalPlan},
};
use uqa_core::memory::OwnedSet;

impl Copying<'_> {
    pub(super) fn lower_query_fields(
        &mut self,
        statement: &SelectStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<QueryPlan> {
        let mut scope = self.enter_plan_node()?;
        let copying = &mut *scope;
        copying.check()?;
        let mut query = copying.retain_plan(crate::plan::retirement::empty_query());
        query.analyzed_grouping = copying.field("analyzed_grouping", |_| Ok(None))?;
        query.relation_privilege_subject =
            copying.field("relation_privilege_subject", |_| Ok(None))?;
        query.ctes = copying.field("ctes", |copying| {
            copying.map_plans(statement.with.iter(), |copying, cte| {
                copying.lower_cte(cte, aggregates)
            })
        })?;
        query.root = copying.field("root", |copying| {
            copying.lower_relational(statement, aggregates)
        })?;
        Ok(query.take())
    }

    pub(super) fn lower_values_query(
        &mut self,
        rows: &[Vec<Expr>],
        aggregates: &dyn AggregateClassifier,
    ) -> Result<QueryPlan> {
        let mut scope = self.enter_plan_node()?;
        let copying = &mut *scope;
        let mut query = copying.retain_plan(crate::plan::retirement::empty_query());
        query.analyzed_grouping = copying.field("analyzed_grouping", |_| Ok(None))?;
        query.relation_privilege_subject =
            copying.field("relation_privilege_subject", |_| Ok(None))?;
        query.ctes = copying.field("ctes", |_| Ok(Vec::new()))?;
        query.root = copying.field("root", |copying| copying.lower_values(rows, aggregates))?;
        Ok(query.take())
    }

    fn lower_values(
        &mut self,
        rows: &[Vec<Expr>],
        aggregates: &dyn AggregateClassifier,
    ) -> Result<RelationalPlan> {
        let mut arena = self.query_arena();
        let rows = Pending::new(self.field("rows", |copying| {
            copying.lower_rows(rows, aggregates, &mut arena)
        })?);
        let subqueries = self.take_arena(arena)?;
        Ok(RelationalPlan::Values {
            rows: rows.take(),
            subqueries,
        })
    }

    fn lower_relational(
        &mut self,
        statement: &SelectStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<RelationalPlan> {
        self.check()?;
        let Some(set) = &statement.set_op else {
            if !statement.values.is_empty() {
                return self.lower_values(&statement.values, aggregates);
            }
            return Ok(RelationalPlan::QueryBlock(self.lower_boxed(|copying| {
                copying.lower_block(statement, aggregates)
            })?));
        };
        let left = self.field("left", |copying| {
            copying.lower_boxed(|copying| {
                if let Some(left) = &set.left {
                    copying.lower_nested_query(left, aggregates)
                } else {
                    copying.field("query", |copying| {
                        let mut scope = copying.enter_plan_node()?;
                        let copying = &mut *scope;
                        Ok(QueryPlan {
                            analyzed_output: None,
                            analyzed_source: None,
                            analyzed_grouping: copying.field("analyzed_grouping", |_| Ok(None))?,
                            relation_privilege_subject: copying
                                .field("relation_privilege_subject", |_| Ok(None))?,
                            relations_bound: false,
                            ctes: copying.field("ctes", |_| Ok(Vec::new()))?,
                            root: copying.field("root", |copying| {
                                Ok(RelationalPlan::QueryBlock(copying.lower_boxed(
                                    |copying| copying.lower_block(statement, aggregates),
                                )?))
                            })?,
                        })
                    })
                }
            })
        })?;
        let left = self.retain_plan(left);
        let right = self.field("right", |copying| {
            copying.lower_boxed(|copying| copying.lower_nested_query(&set.right, aggregates))
        })?;
        let right = self.retain_plan(right);
        let mut arena = self.query_arena();
        let order_by = Pending::new(self.field("order_by", |copying| {
            copying.lower_orders(&set.combined_order_by, aggregates, &mut arena)
        })?);
        let limit = Pending::new(self.field("limit", |copying| {
            copying.lower_optional_scalar_box(set.combined_limit.as_ref(), aggregates, &mut arena)
        })?);
        let offset = Pending::new(self.field("offset", |copying| {
            copying.lower_optional_scalar_box(set.combined_offset.as_ref(), aggregates, &mut arena)
        })?);
        let subqueries = self.take_arena(arena)?;
        Ok(RelationalPlan::SetOp {
            kind: set.kind,
            all: set.all,
            left: left.take(),
            right: right.take(),
            order_by: order_by.take(),
            limit: limit.take(),
            with_ties: set.combined_with_ties,
            offset: offset.take(),
            subqueries,
        })
    }

    fn lower_block(
        &mut self,
        statement: &SelectStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<QueryBlockPlan> {
        self.field("block", |copying| {
            copying.lower_block_fields(statement, aggregates)
        })
    }

    fn lower_block_fields(
        &mut self,
        statement: &SelectStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<QueryBlockPlan> {
        let mut arena = self.query_arena();
        // Scalar-subquery indices follow the established lowering order, including windows after projection roots.
        let mut block =
            self.retain_plan(crate::plan::mutable::construction::empty_projection_block());
        block.projections = self.field("projections", |copying| {
            copying.lower_projections(&statement.projections, aggregates, &mut arena)
        })?;
        block.windows = self.field("windows", |copying| {
            copying.lower_windows(&statement.windows, aggregates, &mut arena)
        })?;
        block.deferred_outer_rewrites = self.field("deferred_outer_rewrites", |copying| {
            copying.map(std::iter::empty(), |_, rewrite| Ok(rewrite))
        })?;
        block.privilege_columns = self.field("privilege_columns", |_| Ok(OwnedSet::new()))?;
        block.from = self.field("from", |copying| {
            statement
                .from
                .as_ref()
                .map(|source| copying.lower_source(source, aggregates, &mut arena))
                .transpose()
        })?;
        block.r#where = self.field("where", |copying| {
            copying.lower_optional_scalar(statement.r#where.as_ref(), aggregates, &mut arena)
        })?;
        block.group_by = self.field("group_by", |copying| {
            copying.lower_scalars(&statement.group_by, aggregates, &mut arena)
        })?;
        block.grouping_sets = self.field("grouping_sets", |copying| {
            copying.lower_rows(&statement.grouping_sets, aggregates, &mut arena)
        })?;
        block.group_distinct = statement.group_distinct;
        block.having = self.field("having", |copying| {
            copying.lower_optional_scalar(statement.having.as_ref(), aggregates, &mut arena)
        })?;
        block.order_by = self.field("order_by", |copying| {
            copying.lower_orders(&statement.order_by, aggregates, &mut arena)
        })?;
        block.limit = self.field("limit", |copying| {
            copying.lower_optional_scalar(statement.limit.as_ref(), aggregates, &mut arena)
        })?;
        block.with_ties = statement.with_ties;
        block.offset = self.field("offset", |copying| {
            copying.lower_optional_scalar(statement.offset.as_ref(), aggregates, &mut arena)
        })?;
        block.distinct = statement.distinct;
        block.distinct_on = self.field("distinct_on", |copying| {
            copying.lower_scalars(&statement.distinct_on, aggregates, &mut arena)
        })?;
        block.subqueries = self.take_arena(arena)?;
        block.locking = self.field("locking", |copying| statement.locking.copy_syntax(copying))?;
        block.compute = block.compute_kind_with_control(aggregates, &self.control)?;
        Ok(block.take())
    }
}
