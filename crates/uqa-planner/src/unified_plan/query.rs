//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SELECT, source, projection, order, CTE, and mutation-child lowering.

use super::rewrite::rewrite_query_scalars;
use super::scalar::{is_builtin_aggregate, lower_scalar_expression};
use super::{
    AccessPathPlan, AggregateClassifier, AssignmentPlan, ComputePlan, CteCyclePlan, CtePlan,
    CteSearchPlan, Expr, ExpressionPlan, FromClause, JoinExecutionStrategy, MergeWhenPlan,
    NoRegisteredAggregates, OrderBy, OrderPlan, Projection, ProjectionPlan, QueryBlockPlan,
    QueryPlan, RelationalPlan, ScalarExpr, SelectStmt, SourcePlan, TableFunctionPlan, CTE,
};

impl QueryPlan {
    /// Rewrite every physical scalar node owned by this query exactly once,
    /// including CTEs, relational sources, and scalar-subquery plans.
    pub fn rewrite_scalar_expressions(&mut self, rewrite: &mut dyn FnMut(&mut ScalarExpr)) {
        rewrite_query_scalars(self, rewrite);
    }

    #[must_use]
    pub fn lower(statement: SelectStmt) -> Self {
        Self::lower_with(statement, &NoRegisteredAggregates)
    }

    #[must_use]
    pub fn lower_with(mut statement: SelectStmt, aggregates: &dyn AggregateClassifier) -> Self {
        let ctes = lower_ctes(&statement.with, aggregates);
        statement.with.clear();
        let root = lower_relational_root(statement, aggregates);
        Self { ctes, root }
    }
}

pub(super) fn lower_ctes(ctes: &[CTE], aggregates: &dyn AggregateClassifier) -> Vec<CtePlan> {
    ctes.iter()
        .map(|cte| CtePlan {
            name: cte.name.clone(),
            columns: cte.columns.clone(),
            recursive: cte.recursive,
            materialization: cte.materialization,
            search: cte.search.as_ref().map(|search| CteSearchPlan {
                columns: search.columns.clone(),
                breadth_first: search.breadth_first,
                sequence_column: search.sequence_column.clone(),
            }),
            cycle: cte.cycle.as_ref().map(|cycle| CteCyclePlan {
                columns: cycle.columns.clone(),
                mark_column: cycle.mark_column.clone(),
                mark_value: lower_scalar_expression(
                    cycle.mark_value.clone(),
                    aggregates,
                    &mut Vec::new(),
                ),
                mark_default: lower_scalar_expression(
                    cycle.mark_default.clone(),
                    aggregates,
                    &mut Vec::new(),
                ),
                path_column: cycle.path_column.clone(),
            }),
            query: Box::new(QueryPlan::lower_with((*cte.query).clone(), aggregates)),
        })
        .collect()
}

pub(super) fn lower_assignments(
    assignments: Vec<(String, Expr)>,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> Vec<AssignmentPlan> {
    assignments
        .into_iter()
        .map(|(column, expression)| AssignmentPlan {
            column,
            value: lower_scalar_expression(expression, aggregates, subqueries),
        })
        .collect()
}

pub(super) fn lower_merge_when(
    clause: uqa_sql::ast::MergeWhen,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> MergeWhenPlan {
    let mut lower_optional = |expression: Option<Expr>| {
        expression.map(|expression| lower_scalar_expression(expression, aggregates, subqueries))
    };
    match clause {
        uqa_sql::ast::MergeWhen::UpdateMatched {
            condition,
            assignments,
        } => {
            let condition = lower_optional(condition);
            let assignments = lower_assignments(assignments, aggregates, subqueries);
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            }
        }
        uqa_sql::ast::MergeWhen::DeleteMatched { condition } => MergeWhenPlan::DeleteMatched {
            condition: lower_optional(condition),
        },
        uqa_sql::ast::MergeWhen::UpdateNotMatchedBySource {
            condition,
            assignments,
        } => {
            let condition = lower_optional(condition);
            let assignments = lower_assignments(assignments, aggregates, subqueries);
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            }
        }
        uqa_sql::ast::MergeWhen::DeleteNotMatchedBySource { condition } => {
            MergeWhenPlan::DeleteNotMatchedBySource {
                condition: lower_optional(condition),
            }
        }
        uqa_sql::ast::MergeWhen::InsertNotMatched {
            condition,
            columns,
            values,
        } => {
            let condition = lower_optional(condition);
            let values = values
                .into_iter()
                .map(|value| lower_scalar_expression(value, aggregates, subqueries))
                .collect();
            MergeWhenPlan::InsertNotMatched {
                condition,
                columns,
                values,
            }
        }
        uqa_sql::ast::MergeWhen::NothingMatched { condition } => MergeWhenPlan::NothingMatched {
            condition: lower_optional(condition),
        },
        uqa_sql::ast::MergeWhen::NothingNotMatched { condition } => {
            MergeWhenPlan::NothingNotMatched {
                condition: lower_optional(condition),
            }
        }
        uqa_sql::ast::MergeWhen::NothingNotMatchedBySource { condition } => {
            MergeWhenPlan::NothingNotMatchedBySource {
                condition: lower_optional(condition),
            }
        }
    }
}
pub(super) fn lower_relational_root(
    mut statement: SelectStmt,
    aggregates: &dyn AggregateClassifier,
) -> RelationalPlan {
    if statement.set_op.is_none() && !statement.values.is_empty() {
        let mut subqueries = Vec::new();
        let rows = statement
            .values
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries))
                    .collect()
            })
            .collect();
        return RelationalPlan::Values { rows, subqueries };
    }
    let Some(set_op) = statement.set_op.take() else {
        return RelationalPlan::QueryBlock(Box::new(QueryBlockPlan::lower_with(
            statement, aggregates,
        )));
    };

    let left = if let Some(left) = set_op.left {
        QueryPlan::lower_with(*left, aggregates)
    } else {
        QueryPlan {
            ctes: Vec::new(),
            root: RelationalPlan::QueryBlock(Box::new(QueryBlockPlan::lower_with(
                statement, aggregates,
            ))),
        }
    };
    let right = QueryPlan::lower_with(set_op.right, aggregates);
    let mut subqueries = Vec::new();
    RelationalPlan::SetOp {
        kind: set_op.kind,
        all: set_op.all,
        left: Box::new(left),
        right: Box::new(right),
        order_by: set_op
            .combined_order_by
            .into_iter()
            .map(|order| OrderPlan::lower_with(order, aggregates, &mut subqueries))
            .collect(),
        limit: set_op
            .combined_limit
            .map(|expr| Box::new(lower_scalar_expression(expr, aggregates, &mut subqueries))),
        with_ties: set_op.combined_with_ties,
        offset: set_op
            .combined_offset
            .map(|expr| Box::new(lower_scalar_expression(expr, aggregates, &mut subqueries))),
        subqueries,
    }
}

impl QueryBlockPlan {
    fn lower_with(statement: SelectStmt, aggregates: &dyn AggregateClassifier) -> Self {
        debug_assert!(statement.with.is_empty());
        debug_assert!(statement.set_op.is_none());
        let mut subqueries = Vec::new();
        let projections: Vec<ProjectionPlan> = statement
            .projections
            .into_iter()
            .map(|projection| ProjectionPlan::lower_with(projection, aggregates, &mut subqueries))
            .collect();
        let is_aggregate =
            |name: &str| is_builtin_aggregate(name) || aggregates.is_registered_aggregate(name);
        let has_aggregate = !statement.group_by.is_empty()
            || !statement.grouping_sets.is_empty()
            || statement.having.is_some()
            || projections
                .iter()
                .any(|projection| projection.expr.contains_aggregate(&is_aggregate));
        let has_window = projections
            .iter()
            .any(|projection| projection.expr.contains_window());
        let compute = if has_aggregate {
            ComputePlan::Aggregate
        } else if has_window {
            ComputePlan::Window
        } else {
            ComputePlan::Project
        };
        Self {
            projections,
            from: statement
                .from
                .map(|source| SourcePlan::lower_with(source, aggregates, &mut subqueries)),
            r#where: statement
                .r#where
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries)),
            compute,
            group_by: statement
                .group_by
                .into_iter()
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries))
                .collect(),
            grouping_sets: statement
                .grouping_sets
                .into_iter()
                .map(|set| {
                    set.into_iter()
                        .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries))
                        .collect()
                })
                .collect(),
            group_distinct: statement.group_distinct,
            having: statement
                .having
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries)),
            order_by: statement
                .order_by
                .into_iter()
                .map(|order| OrderPlan::lower_with(order, aggregates, &mut subqueries))
                .collect(),
            limit: statement
                .limit
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries)),
            with_ties: statement.with_ties,
            offset: statement
                .offset
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries)),
            distinct: statement.distinct,
            distinct_on: statement
                .distinct_on
                .into_iter()
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries))
                .collect(),
            subqueries,
            access: AccessPathPlan::Row,
            locking: statement.locking,
        }
    }

    /// Expression nodes evaluated while executing this query block. Query
    /// bodies under `FROM (SELECT ...)` are excluded because their child plan
    /// installs its own expression scope when it executes.
    #[must_use]
    pub fn expressions(&self) -> Vec<&ScalarExpr> {
        let mut expressions = Vec::new();
        if let Some(source) = &self.from {
            source.push_expressions(&mut expressions);
        }
        if let Some(filter) = &self.r#where {
            expressions.push(filter);
        }
        for projection in &self.projections {
            expressions.push(&projection.expr);
        }
        expressions.extend(&self.group_by);
        for set in &self.grouping_sets {
            expressions.extend(set);
        }
        if let Some(having) = &self.having {
            expressions.push(having);
        }
        expressions.extend(self.order_by.iter().map(|order| &order.expr));
        if let Some(limit) = &self.limit {
            expressions.push(limit);
        }
        if let Some(offset) = &self.offset {
            expressions.push(offset);
        }
        expressions.extend(&self.distinct_on);
        expressions
    }
}

impl SourcePlan {
    /// SQL-visible relation qualifier for a non-join FROM item. PostgreSQL uses the local function name, not its schema-qualified lookup identity, when a table function has no explicit alias.
    #[must_use]
    pub fn visible_qualifier(&self) -> Option<&str> {
        match self {
            Self::Table {
                qualifier, alias, ..
            } => Some(alias.as_deref().unwrap_or(qualifier)),
            Self::Function {
                output_name, alias, ..
            } => Some(alias.as_deref().unwrap_or(output_name)),
            Self::FunctionGroup {
                functions, alias, ..
            } => alias.as_deref().or_else(|| {
                functions
                    .first()
                    .map(|function| function.output_name.as_str())
            }),
            Self::Values {
                alias,
                internal_relation,
                ..
            } => internal_relation
                .is_none()
                .then_some(alias.as_deref())
                .flatten(),
            Self::Subquery { alias, .. } => alias.as_deref(),
            Self::Join { alias, .. } => alias.as_deref(),
        }
    }

    pub(super) fn lower_with(
        source: FromClause,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Self {
        match source {
            FromClause::Table {
                name,
                qualifier,
                alias,
                include_descendants,
            } => Self::Table {
                name,
                qualifier,
                alias,
                include_descendants,
            },
            FromClause::Join {
                left,
                right,
                kind,
                on,
                using,
                natural,
                alias,
                column_aliases,
                lateral,
            } => Self::Join {
                left: Box::new(Self::lower_with(*left, aggregates, subqueries)),
                right: Box::new(Self::lower_with(*right, aggregates, subqueries)),
                kind,
                on: on.map(|expr| lower_scalar_expression(expr, aggregates, subqueries)),
                using,
                natural,
                alias,
                column_aliases,
                lateral,
                strategy: JoinExecutionStrategy::Auto,
            },
            FromClause::Values {
                rows,
                alias,
                column_aliases,
                internal_relation,
                internal_column_types,
            } => Self::Values {
                rows: rows
                    .into_iter()
                    .map(|row| {
                        row.into_iter()
                            .map(|expr| lower_scalar_expression(expr, aggregates, subqueries))
                            .collect()
                    })
                    .collect(),
                alias,
                column_aliases,
                internal_relation,
                internal_column_types,
            },
            FromClause::Function {
                name,
                output_name,
                relation,
                args,
                alias,
                column_aliases,
                ordinality,
                column_types,
            } => Self::Function {
                name,
                binding: None,
                output_name,
                relation,
                args: args
                    .into_iter()
                    .map(|expr| lower_scalar_expression(expr, aggregates, subqueries))
                    .collect(),
                alias,
                column_aliases,
                ordinality,
                column_types,
            },
            FromClause::FunctionGroup {
                functions,
                alias,
                column_aliases,
                ordinality,
            } => Self::FunctionGroup {
                functions: functions
                    .into_iter()
                    .map(|function| TableFunctionPlan {
                        name: function.name,
                        binding: None,
                        output_name: function.output_name,
                        relation: function.relation,
                        args: function
                            .args
                            .into_iter()
                            .map(|expr| lower_scalar_expression(expr, aggregates, subqueries))
                            .collect(),
                        column_aliases: function.column_aliases,
                        column_types: function.column_types,
                    })
                    .collect(),
                alias,
                column_aliases,
                ordinality,
            },
            FromClause::Subquery {
                body,
                alias,
                column_aliases,
            } => Self::Subquery {
                body: Box::new(QueryPlan::lower_with(*body, aggregates)),
                alias,
                column_aliases,
            },
        }
    }

    fn push_expressions<'a>(&'a self, output: &mut Vec<&'a ScalarExpr>) {
        match self {
            Self::Table { .. } | Self::Subquery { .. } => {}
            Self::Join {
                left, right, on, ..
            } => {
                left.push_expressions(output);
                right.push_expressions(output);
                if let Some(on) = on {
                    output.push(on);
                }
            }
            Self::Values { rows, .. } => {
                for row in rows {
                    output.extend(row);
                }
            }
            Self::Function { args, .. } => output.extend(args),
            Self::FunctionGroup { functions, .. } => {
                for function in functions {
                    output.extend(&function.args);
                }
            }
        }
    }

    pub fn collect_tables(&self, output: &mut Vec<(String, Option<String>)>) {
        match self {
            Self::Table {
                name,
                qualifier,
                alias,
                ..
            } => output.push((
                name.clone(),
                Some(alias.as_ref().unwrap_or(qualifier).clone()),
            )),
            Self::Join { left, right, .. } => {
                left.collect_tables(output);
                right.collect_tables(output);
            }
            Self::Values { .. }
            | Self::Function { .. }
            | Self::FunctionGroup { .. }
            | Self::Subquery { .. } => {}
        }
    }
}

impl ProjectionPlan {
    pub(super) fn lower_with(
        projection: Projection,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Self {
        Self {
            expr: lower_scalar_expression(projection.expr, aggregates, subqueries),
            alias: projection.alias,
        }
    }
}

impl OrderPlan {
    fn lower_with(
        order: OrderBy,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Self {
        Self {
            expr: lower_scalar_expression(order.expr, aggregates, subqueries),
            descending: order.descending,
            nulls: order.nulls,
        }
    }
}

impl ExpressionPlan {
    #[must_use]
    pub fn lower(expression: Expr) -> Self {
        Self::lower_with(expression, &NoRegisteredAggregates)
    }

    pub(super) fn lower_with(expression: Expr, aggregates: &dyn AggregateClassifier) -> Self {
        let mut subqueries = Vec::new();
        let scalar = lower_scalar_expression(expression, aggregates, &mut subqueries);
        Self { scalar, subqueries }
    }
}
