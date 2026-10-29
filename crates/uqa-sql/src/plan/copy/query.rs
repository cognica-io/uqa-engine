//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result};
use crate::plan::{
    CteCyclePlan, CtePlan, CtePlanBody, CteSearchPlan, OrderPlan, ProjectionPlan, QueryBlockPlan,
    QueryPlan, RelationalPlan,
};
use crate::{ScalarFrameBound, ScalarWindowFrame, ScalarWindowSpec};

impl Copying<'_> {
    pub(super) fn query(&mut self, query: &QueryPlan) -> Result<QueryPlan> {
        self.field("query", |copying| copying.query_fields(query))
    }

    pub(in crate::plan) fn query_fields(&mut self, query: &QueryPlan) -> Result<QueryPlan> {
        self.check()?;
        Ok(QueryPlan {
            analyzed_output: query.analyzed_output.clone(),
            analyzed_source: query.analyzed_source.clone(),
            analyzed_grouping: self.field("analyzed_grouping", |copying| {
                query
                    .analyzed_grouping
                    .as_ref()
                    .map(|grouping| grouping.copy_with(copying))
                    .transpose()
            })?,
            relations_bound: query.relations_bound,
            relation_privilege_subject: self.role(query.relation_privilege_subject.as_ref())?,
            ctes: self.field("ctes", |copying| copying.map(query.ctes.iter(), Self::cte))?,
            root: self.field("root", |copying| copying.relational(&query.root))?,
        })
    }

    pub(super) fn cte(&mut self, cte: &CtePlan) -> Result<CtePlan> {
        Ok(CtePlan {
            name: self.text(&cte.name)?,
            columns: self.texts(&cte.columns)?,
            recursive: cte.recursive,
            materialization: cte.materialization,
            search: cte
                .search
                .as_ref()
                .map(|search| {
                    Ok::<_, crate::SQLError>(CteSearchPlan {
                        columns: self.texts(&search.columns)?,
                        breadth_first: search.breadth_first,
                        sequence_column: self.text(&search.sequence_column)?,
                    })
                })
                .transpose()?,
            cycle: cte
                .cycle
                .as_ref()
                .map(|cycle| {
                    Ok::<_, crate::SQLError>(CteCyclePlan {
                        columns: self.texts(&cycle.columns)?,
                        mark_column: self.text(&cycle.mark_column)?,
                        mark_value: self.scalar(&cycle.mark_value)?,
                        mark_default: self.scalar(&cycle.mark_default)?,
                        path_column: self.text(&cycle.path_column)?,
                    })
                })
                .transpose()?,
            body: self.field("body", |copying| {
                Ok(match &cte.body {
                    CtePlanBody::Query(query) => {
                        CtePlanBody::Query(copying.boxed(|copying| copying.query(query))?)
                    }
                    CtePlanBody::Command(command) => {
                        CtePlanBody::Command(copying.boxed(|copying| {
                            copying.field("command", |copying| copying.command_syntax(command))
                        })?)
                    }
                })
            })?,
        })
    }

    fn relational(&mut self, root: &RelationalPlan) -> Result<RelationalPlan> {
        self.check()?;
        Ok(match root {
            RelationalPlan::QueryBlock(block) => {
                RelationalPlan::QueryBlock(self.boxed(|copying| copying.block(block))?)
            }
            RelationalPlan::Values { rows, subqueries } => RelationalPlan::Values {
                rows: self.field("rows", |copying| copying.rows(rows))?,
                subqueries: self.field("subqueries", |copying| {
                    copying.map(subqueries.iter(), Self::query)
                })?,
            },
            RelationalPlan::SetOp {
                kind,
                all,
                left,
                right,
                order_by,
                limit,
                with_ties,
                offset,
                subqueries,
            } => RelationalPlan::SetOp {
                kind: *kind,
                all: *all,
                left: self.field("left", |copying| {
                    copying.boxed(|copying| copying.query(left))
                })?,
                right: self.field("right", |copying| {
                    copying.boxed(|copying| copying.query(right))
                })?,
                order_by: self.field("order_by", |copying| {
                    copying.map(order_by.iter(), Self::order)
                })?,
                limit: self.field("limit", |copying| {
                    copying.optional_scalar_box(limit.as_deref())
                })?,
                with_ties: *with_ties,
                offset: self.field("offset", |copying| {
                    copying.optional_scalar_box(offset.as_deref())
                })?,
                subqueries: self.field("subqueries", |copying| {
                    copying.map(subqueries.iter(), Self::query)
                })?,
            },
        })
    }

    fn block(&mut self, block: &QueryBlockPlan) -> Result<QueryBlockPlan> {
        self.field("block", |copying| copying.block_fields(block))
    }

    fn block_fields(&mut self, block: &QueryBlockPlan) -> Result<QueryBlockPlan> {
        Ok(QueryBlockPlan {
            deferred_outer_rewrites: self
                .map(block.deferred_outer_rewrites.iter(), |copying, rewrite| {
                    rewrite.copy_with(copying)
                })?,
            windows: self.field("windows", |copying| {
                copying.map(block.windows.iter(), |copying, window| {
                    Ok(crate::ast::WindowDefinition {
                        name: copying.optional_text(window.name.as_deref())?,
                        inherited: window.inherited,
                        spec: copying.field("spec", |copying| copying.window(&window.spec))?,
                    })
                })
            })?,
            projections: self.field("projections", |copying| {
                copying.map(block.projections.iter(), Self::projection)
            })?,
            privilege_columns: self.set(&block.privilege_columns, Self::identity)?,
            from: self.field("from", |copying| {
                block
                    .from
                    .as_ref()
                    .map(|source| copying.source(source))
                    .transpose()
            })?,
            r#where: self.field("where", |copying| {
                copying.optional_scalar(block.r#where.as_ref())
            })?,
            compute: block.compute.clone(),
            group_by: self.field("group_by", |copying| {
                copying.map(block.group_by.iter(), Self::scalar)
            })?,
            grouping_sets: self.field("grouping_sets", |copying| {
                copying.rows(&block.grouping_sets)
            })?,
            group_distinct: block.group_distinct,
            having: self.field("having", |copying| {
                copying.optional_scalar(block.having.as_ref())
            })?,
            order_by: self.field("order_by", |copying| {
                copying.map(block.order_by.iter(), Self::order)
            })?,
            limit: self.field("limit", |copying| {
                copying.optional_scalar(block.limit.as_ref())
            })?,
            with_ties: block.with_ties,
            offset: self.field("offset", |copying| {
                copying.optional_scalar(block.offset.as_ref())
            })?,
            distinct: block.distinct,
            distinct_on: self.field("distinct_on", |copying| {
                copying.map(block.distinct_on.iter(), Self::scalar)
            })?,
            subqueries: self.field("subqueries", |copying| {
                copying.map(block.subqueries.iter(), Self::query)
            })?,
            access: block.access.clone(),
            locking: self.map(block.locking.iter(), |copying, clause| {
                Ok(crate::ast::LockingClause {
                    strength: clause.strength,
                    wait: clause.wait,
                    relations: copying.texts(&clause.relations)?,
                })
            })?,
        })
    }

    pub(super) fn rows(
        &mut self,
        rows: &[Vec<crate::ScalarExpr>],
    ) -> Result<Vec<Vec<crate::ScalarExpr>>> {
        self.map(rows.iter(), |copying, row| {
            copying.map(row.iter(), Self::scalar)
        })
    }

    pub(super) fn projection(&mut self, projection: &ProjectionPlan) -> Result<ProjectionPlan> {
        Ok(ProjectionPlan {
            expr: self.scalar(&projection.expr)?,
            alias: self.field("alias", |copying| {
                copying.optional_text(projection.alias.as_deref())
            })?,
        })
    }

    fn order(&mut self, order: &OrderPlan) -> Result<OrderPlan> {
        Ok(OrderPlan {
            expr: self.scalar(&order.expr)?,
            descending: order.descending,
            nulls: order.nulls,
        })
    }

    fn window(&mut self, window: &ScalarWindowSpec) -> Result<ScalarWindowSpec> {
        Ok(ScalarWindowSpec {
            definition: window.definition,
            partition_by: self.field("partition_by", |copying| {
                copying.map(window.partition_by.iter(), Self::scalar)
            })?,
            order_by: self.field("order_by", |copying| {
                copying.map(window.order_by.iter(), |copying, order| {
                    Ok(crate::ScalarOrder {
                        expr: copying.scalar(&order.expr)?,
                        descending: order.descending,
                        nulls: order.nulls,
                    })
                })
            })?,
            frame: self.field("frame", |copying| {
                window
                    .frame
                    .as_ref()
                    .map(|frame| {
                        Ok::<_, crate::SQLError>(ScalarWindowFrame {
                            mode: frame.mode,
                            start: copying.field("start", |copying| copying.bound(&frame.start))?,
                            end: copying.field("end", |copying| copying.bound(&frame.end))?,
                            between: frame.between,
                            exclusion: frame.exclusion,
                        })
                    })
                    .transpose()
            })?,
        })
    }

    fn bound(&mut self, bound: &ScalarFrameBound) -> Result<ScalarFrameBound> {
        Ok(match bound {
            ScalarFrameBound::UnboundedPreceding => ScalarFrameBound::UnboundedPreceding,
            ScalarFrameBound::UnboundedFollowing => ScalarFrameBound::UnboundedFollowing,
            ScalarFrameBound::CurrentRow => ScalarFrameBound::CurrentRow,
            ScalarFrameBound::Preceding(value) => {
                ScalarFrameBound::Preceding(self.scalar_box(value)?)
            }
            ScalarFrameBound::Following(value) => {
                ScalarFrameBound::Following(self.scalar_box(value)?)
            }
        })
    }
}
