//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, QueryArena, Result, SyntaxCopy};
use crate::{
    ast::{Expr, SelectStmt, WindowDefinition},
    plan::{
        AccessPathPlan, AggregateClassifier, ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan,
    },
};
use uqa_core::memory::OwnedSet;

impl Copying<'_> {
    pub(super) fn lower_query_fields(
        &mut self,
        statement: &SelectStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<QueryPlan> {
        self.check()?;
        Ok(QueryPlan {
            analyzed_output: None,
            analyzed_source: None,
            analyzed_grouping: self.field("analyzed_grouping", |_| Ok(None))?,
            relation_privilege_subject: self.field("relation_privilege_subject", |_| Ok(None))?,
            relations_bound: false,
            ctes: self.field("ctes", |copying| {
                copying.map(statement.with.iter(), |copying, cte| {
                    copying.lower_cte(cte, aggregates)
                })
            })?,
            root: self.field("root", |copying| {
                copying.lower_relational(statement, aggregates)
            })?,
        })
    }

    pub(super) fn lower_values_query(
        &mut self,
        rows: &[Vec<Expr>],
        aggregates: &dyn AggregateClassifier,
    ) -> Result<QueryPlan> {
        Ok(QueryPlan {
            analyzed_output: None,
            analyzed_source: None,
            analyzed_grouping: self.field("analyzed_grouping", |_| Ok(None))?,
            relation_privilege_subject: self.field("relation_privilege_subject", |_| Ok(None))?,
            relations_bound: false,
            ctes: self.field("ctes", |_| Ok(Vec::new()))?,
            root: self.field("root", |copying| copying.lower_values(rows, aggregates))?,
        })
    }

    fn lower_values(
        &mut self,
        rows: &[Vec<Expr>],
        aggregates: &dyn AggregateClassifier,
    ) -> Result<RelationalPlan> {
        let mut arena = QueryArena::new(self.control, self.parameters);
        Ok(RelationalPlan::Values {
            rows: self.field("rows", |copying| {
                copying.lower_rows(rows, aggregates, &mut arena)
            })?,
            subqueries: self.take_arena(arena)?,
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
            return Ok(RelationalPlan::QueryBlock(
                self.boxed(|copying| copying.lower_block(statement, aggregates))?,
            ));
        };
        let left = self.field("left", |copying| {
            copying.boxed(|copying| {
                if let Some(left) = &set.left {
                    copying.lower_nested_query(left, aggregates)
                } else {
                    copying.field("query", |copying| {
                        Ok(QueryPlan {
                            analyzed_output: None,
                            analyzed_source: None,
                            analyzed_grouping: copying.field("analyzed_grouping", |_| Ok(None))?,
                            relation_privilege_subject: copying
                                .field("relation_privilege_subject", |_| Ok(None))?,
                            relations_bound: false,
                            ctes: copying.field("ctes", |_| Ok(Vec::new()))?,
                            root: copying.field("root", |copying| {
                                Ok(RelationalPlan::QueryBlock(copying.boxed(|copying| {
                                    copying.lower_block(statement, aggregates)
                                })?))
                            })?,
                        })
                    })
                }
            })
        })?;
        let right = self.field("right", |copying| {
            copying.boxed(|copying| copying.lower_nested_query(&set.right, aggregates))
        })?;
        let mut arena = QueryArena::new(self.control, self.parameters);
        Ok(RelationalPlan::SetOp {
            kind: set.kind,
            all: set.all,
            left,
            right,
            order_by: self.field("order_by", |copying| {
                copying.map(set.combined_order_by.iter(), |copying, order| {
                    copying.lower_order(order, aggregates, &mut arena)
                })
            })?,
            limit: self.field("limit", |copying| {
                copying.lower_optional_scalar_box(
                    set.combined_limit.as_ref(),
                    aggregates,
                    &mut arena,
                )
            })?,
            with_ties: set.combined_with_ties,
            offset: self.field("offset", |copying| {
                copying.lower_optional_scalar_box(
                    set.combined_offset.as_ref(),
                    aggregates,
                    &mut arena,
                )
            })?,
            subqueries: self.take_arena(arena)?,
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
        let mut arena = QueryArena::new(self.control, self.parameters);
        // Scalar-subquery indices follow the established lowering order, including windows after projection roots.
        let projections = self.field("projections", |copying| {
            copying.map(statement.projections.iter(), |copying, projection| {
                copying.lower_projection(projection, aggregates, &mut arena)
            })
        })?;
        let windows = self.field("windows", |copying| {
            copying.map(statement.windows.iter(), |copying, window| {
                Ok(WindowDefinition {
                    name: copying.optional_text(window.name.as_deref())?,
                    inherited: window.inherited,
                    spec: copying.field("spec", |copying| {
                        copying.lower_window(&window.spec, aggregates, &mut arena)
                    })?,
                })
            })
        })?;
        let mut block = QueryBlockPlan {
            deferred_outer_rewrites: self.field("deferred_outer_rewrites", |copying| {
                copying.map(std::iter::empty(), |_, rewrite| Ok(rewrite))
            })?,
            windows,
            projections,
            privilege_columns: self.field("privilege_columns", |_| Ok(OwnedSet::new()))?,
            from: self.field("from", |copying| {
                statement
                    .from
                    .as_ref()
                    .map(|source| copying.lower_source(source, aggregates, &mut arena))
                    .transpose()
            })?,
            r#where: self.field("where", |copying| {
                copying.lower_optional_scalar(statement.r#where.as_ref(), aggregates, &mut arena)
            })?,
            compute: ComputePlan::Project,
            group_by: self.field("group_by", |copying| {
                copying.map(statement.group_by.iter(), |copying, expression| {
                    copying.lower_scalar(expression, aggregates, &mut arena)
                })
            })?,
            grouping_sets: self.field("grouping_sets", |copying| {
                copying.lower_rows(&statement.grouping_sets, aggregates, &mut arena)
            })?,
            group_distinct: statement.group_distinct,
            having: self.field("having", |copying| {
                copying.lower_optional_scalar(statement.having.as_ref(), aggregates, &mut arena)
            })?,
            order_by: self.field("order_by", |copying| {
                copying.map(statement.order_by.iter(), |copying, order| {
                    copying.lower_order(order, aggregates, &mut arena)
                })
            })?,
            limit: self.field("limit", |copying| {
                copying.lower_optional_scalar(statement.limit.as_ref(), aggregates, &mut arena)
            })?,
            with_ties: statement.with_ties,
            offset: self.field("offset", |copying| {
                copying.lower_optional_scalar(statement.offset.as_ref(), aggregates, &mut arena)
            })?,
            distinct: statement.distinct,
            distinct_on: self.field("distinct_on", |copying| {
                copying.map(statement.distinct_on.iter(), |copying, expression| {
                    copying.lower_scalar(expression, aggregates, &mut arena)
                })
            })?,
            subqueries: self.take_arena(arena)?,
            access: AccessPathPlan::Row,
            locking: self.field("locking", |copying| statement.locking.copy_syntax(copying))?,
        };
        block.compute = block.compute_kind_with_control(aggregates, &self.control)?;
        Ok(block)
    }
}
