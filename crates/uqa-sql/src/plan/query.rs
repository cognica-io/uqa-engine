//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SELECT, source, projection, order, CTE, and mutation-child lowering.

use super::rewrite::rewrite_query_scalars;
use super::scalar::lower_scalar_expression;
use super::{
    AccessPathPlan, AggregateClassifier, AssignmentPlan, ComputePlan, CteCyclePlan, CtePlan,
    CteSearchPlan, Expr, ExpressionPlan, FromClause, JoinExecutionStrategy, MergeWhenPlan,
    NoRegisteredAggregates, OrderBy, OrderPlan, Projection, ProjectionPlan, QueryBlockPlan,
    QueryPlan, RelationalPlan, ScalarExpr, SelectStmt, SourcePlan, TableFunctionPlan, CTE,
};

impl QueryBlockPlan {
    /// Select the computation from resolved calls before output-demand rewrites remove unobserved aggregate values.
    pub fn compute_kind(&self, aggregates: &dyn AggregateClassifier) -> ComputePlan {
        let contains_aggregate = |expression: &ScalarExpr| {
            crate::semantics::aggregates::contains_aggregate(aggregates, expression)
        };
        let has_aggregate = !self.group_by.is_empty()
            || !self.grouping_sets.is_empty()
            || self.having.is_some()
            || self
                .projections
                .iter()
                .any(|projection| contains_aggregate(&projection.expr))
            || self
                .windows
                .iter()
                .flat_map(|window| window.spec.expressions())
                .any(contains_aggregate);
        let has_window = self
            .projections
            .iter()
            .any(|projection| projection.expr.contains_window());
        if has_aggregate {
            ComputePlan::Aggregate
        } else if has_window {
            ComputePlan::Window
        } else {
            ComputePlan::Project
        }
    }

    /// One scalar SELECT with no relational clauses or subplans. The caller must also reject set-returning routines using its selected catalog.
    pub fn is_single_expression(&self) -> bool {
        self.deferred_outer_rewrites.is_empty()
            && matches!(self.compute, ComputePlan::Project)
            && self.from.is_none()
            && self.r#where.is_none()
            && self.group_by.is_empty()
            && self.grouping_sets.is_empty()
            && self.having.is_none()
            && self.windows.is_empty()
            && !self.distinct
            && self.distinct_on.is_empty()
            && self.order_by.is_empty()
            && self.limit.is_none()
            && self.offset.is_none()
            && self.locking.is_empty()
            && self.subqueries.is_empty()
            && self.projections.len() == 1
            && {
                let mut window = false;
                self.projections[0]
                    .expr
                    .visit(&mut |node| window |= matches!(node, ScalarExpr::WindowCall { .. }));
                !window
            }
    }
}

impl QueryPlan {
    /// Rewrite every physical scalar node owned by this query exactly once,
    /// including CTEs, relational sources, and scalar-subquery plans.
    pub fn rewrite_scalar_expressions(&mut self, rewrite: &mut dyn FnMut(&mut ScalarExpr)) {
        rewrite_query_scalars(self, rewrite);
    }

    /// Visit every `FROM` item this query owns, including those of nested queries.
    pub fn visit_sources_mut(&mut self, visit: &mut dyn FnMut(&mut SourcePlan)) {
        super::rewrite::visit_query_sources(self, visit);
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
        let mut plan = Self {
            analyzed_output: None,
            analyzed_source: None,
            analyzed_grouping: None,
            relation_privilege_subject: None,
            relations_bound: false,
            ctes,
            root,
        };
        plan.normalize_window_definitions()
            .expect("compiler windows belong to their query block");
        plan
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
            body: super::CtePlanBody::from(super::UnifiedPlan::lower_with(
                cte.body.clone().into_statement(),
                aggregates,
            )),
        })
        .collect()
}

pub(super) fn lower_assignments(
    assignments: Vec<(crate::ast::AssignmentTargets, Expr)>,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> Vec<AssignmentPlan> {
    assignments
        .into_iter()
        .map(|(target, expression)| AssignmentPlan {
            target: target
                .map(|expression| lower_scalar_expression(expression, aggregates, subqueries)),
            value: lower_scalar_expression(expression, aggregates, subqueries),
        })
        .collect()
}

pub(super) fn lower_targets(
    targets: Vec<crate::ast::AssignmentTarget>,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> Vec<crate::ast::AssignmentTarget<ScalarExpr>> {
    targets
        .into_iter()
        .map(|target| {
            target.map(|expression| lower_scalar_expression(expression, aggregates, subqueries))
        })
        .collect()
}

pub(super) fn lower_merge_when(
    clause: crate::ast::MergeWhen,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> MergeWhenPlan {
    let mut lower_optional = |expression: Option<Expr>| {
        expression.map(|expression| lower_scalar_expression(expression, aggregates, subqueries))
    };
    match clause {
        crate::ast::MergeWhen::UpdateMatched {
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
        crate::ast::MergeWhen::DeleteMatched { condition } => MergeWhenPlan::DeleteMatched {
            condition: lower_optional(condition),
        },
        crate::ast::MergeWhen::UpdateNotMatchedBySource {
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
        crate::ast::MergeWhen::DeleteNotMatchedBySource { condition } => {
            MergeWhenPlan::DeleteNotMatchedBySource {
                condition: lower_optional(condition),
            }
        }
        crate::ast::MergeWhen::InsertNotMatched {
            condition,
            columns,
            overriding,
            values,
        } => {
            let condition = lower_optional(condition);
            let values = values
                .into_iter()
                .map(|value| lower_scalar_expression(value, aggregates, subqueries))
                .collect();
            MergeWhenPlan::InsertNotMatched {
                condition,
                columns: lower_targets(columns, aggregates, subqueries),
                overriding,
                values,
            }
        }
        crate::ast::MergeWhen::NothingMatched { condition } => MergeWhenPlan::NothingMatched {
            condition: lower_optional(condition),
        },
        crate::ast::MergeWhen::NothingNotMatched { condition } => {
            MergeWhenPlan::NothingNotMatched {
                condition: lower_optional(condition),
            }
        }
        crate::ast::MergeWhen::NothingNotMatchedBySource { condition } => {
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
            analyzed_output: None,
            analyzed_source: None,
            analyzed_grouping: None,
            relation_privilege_subject: None,
            relations_bound: false,
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
        debug_assert_eq!(statement.with.len(), 0);
        debug_assert!(statement.set_op.is_none());
        let mut subqueries = Vec::new();
        let projections: Vec<ProjectionPlan> = statement
            .projections
            .into_iter()
            .map(|projection| ProjectionPlan::lower_with(projection, aggregates, &mut subqueries))
            .collect();
        let windows: Vec<_> = statement
            .windows
            .into_iter()
            .map(|definition| crate::ast::WindowDefinition {
                name: definition.name,
                inherited: definition.inherited,
                spec: super::scalar::lower_window_spec(
                    definition.spec,
                    aggregates,
                    &mut subqueries,
                ),
            })
            .collect();
        let mut block = Self {
            deferred_outer_rewrites: Vec::new(),
            windows,
            projections,
            privilege_columns: std::collections::BTreeSet::default(),
            from: statement
                .from
                .map(|source| SourcePlan::lower_with(source, aggregates, &mut subqueries)),
            r#where: statement
                .r#where
                .map(|expr| lower_scalar_expression(expr, aggregates, &mut subqueries)),
            compute: ComputePlan::Project,
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
        };
        block.compute = block.compute_kind(aggregates);
        block
    }

    /// Expression nodes evaluated while executing this query block. Query
    /// bodies under `FROM (SELECT ...)` are excluded because their child plan
    /// installs its own expression scope when it executes.
    #[must_use]
    pub fn expressions(&self) -> Vec<&ScalarExpr> {
        let mut expressions = Vec::new();
        self.visit_expressions(&mut |expression| expressions.push(expression));
        expressions
    }

    /// Borrow direct source roots followed by this query's own roots. Derived-query bodies and scalar-query arenas keep their separate namespaces.
    pub fn visit_expressions<'a>(&'a self, visit: &mut dyn FnMut(&'a ScalarExpr)) {
        self.try_visit_expressions(&mut |expression| {
            visit(expression);
            Ok::<_, std::convert::Infallible>(true)
        })
        .unwrap_or_else(|never| match never {});
    }

    /// Visit direct expression roots in initialization order. Return false when the callback stops successfully; an error also stops before the next root.
    pub fn try_visit_expressions<'a, E>(
        &'a self,
        visit: &mut dyn FnMut(&'a ScalarExpr) -> Result<bool, E>,
    ) -> Result<bool, E> {
        if let Some(source) = &self.from {
            if !source.try_visit_expressions(visit)? {
                return Ok(false);
            }
        }
        for expression in self.query_expressions() {
            if !visit(expression)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Expressions at this query level, excluding FROM-item inputs and join conditions, which have their own namespaces.
    pub fn query_expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        // Scan projections precede scan qualifications; aggregate and window outputs follow their input scan.
        let (before, after): (&[ProjectionPlan], &[ProjectionPlan]) =
            if matches!(self.compute, ComputePlan::Project) {
                (&self.projections, &[])
            } else {
                (&[], &self.projections)
            };
        before
            .iter()
            .map(|projection| &projection.expr)
            .chain(self.r#where.as_ref())
            .chain(after.iter().map(|projection| &projection.expr))
            .chain(&self.group_by)
            .chain(self.grouping_sets.iter().flatten())
            .chain(self.having.as_ref())
            .chain(self.order_by.iter().map(|order| &order.expr))
            .chain(self.limit.as_ref())
            .chain(self.offset.as_ref())
            .chain(&self.distinct_on)
            .chain(
                self.windows
                    .iter()
                    .flat_map(|window| window.spec.expressions()),
            )
    }
}

impl SourcePlan {
    /// SQL-visible relation qualifier for a non-join FROM item. `PostgreSQL` uses the local function name, not its schema-qualified lookup identity, when a table function has no explicit alias.
    #[must_use]
    pub fn visible_qualifier(&self) -> Option<&str> {
        match self {
            crate::plan::SourcePlan::RetainedRows(_) => None,
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

    #[expect(
        clippy::too_many_lines,
        reason = "plan lowering preserves exhaustive variants and structural identities"
    )]
    pub(super) fn lower_with(
        source: FromClause,
        aggregates: &dyn AggregateClassifier,
        subqueries: &mut Vec<QueryPlan>,
    ) -> Self {
        match source {
            FromClause::RetainedRows(source) => Self::RetainedRows(source),
            FromClause::Table {
                name,
                qualifier,
                alias,
                column_aliases,
                bound_columns,
                include_descendants,
            } => Self::Table {
                executable_query: None,
                name,
                qualifier,
                alias,
                column_aliases,
                bound_columns,
                include_descendants,
            },
            FromClause::Join {
                comma,
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
                comma,
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
                lateral,
                rows,
                alias,
                column_aliases,
                internal_relation,
                internal_column_types,
                internal_column_collations,
            } => Self::Values {
                lateral,
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
                internal_column_collations,
            },
            FromClause::Function {
                name,
                binding,
                output_name,
                relations,
                args,
                alias,
                column_aliases,
                ordinality,
                column_types,
            } => Self::Function {
                name,
                binding,
                output_name,
                relations,
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
                        binding: function.binding,
                        output_name: function.output_name,
                        relations: function.relations,
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
                lateral,
                body,
                alias,
                column_aliases,
            } => Self::Subquery {
                captured_output: None,
                lateral,
                body: Box::new(QueryPlan::lower_with(*body, aggregates)),
                alias,
                column_aliases,
            },
        }
    }

    /// Visit scalar roots in this source's expression namespace. Derived-query bodies retain their own scope. Return false on a successful callback stop; an error also stops before the next root.
    pub fn try_visit_expressions<'a, E>(
        &'a self,
        visit: &mut dyn FnMut(&'a ScalarExpr) -> Result<bool, E>,
    ) -> Result<bool, E> {
        match self {
            Self::RetainedRows(_) | Self::Table { .. } | Self::Subquery { .. } => {}
            Self::Join {
                left, right, on, ..
            } => {
                if !left.try_visit_expressions(visit)? || !right.try_visit_expressions(visit)? {
                    return Ok(false);
                }
                if let Some(on) = on {
                    if !visit(on)? {
                        return Ok(false);
                    }
                }
            }
            Self::Values { rows, .. } => {
                for expression in rows.iter().flatten() {
                    if !visit(expression)? {
                        return Ok(false);
                    }
                }
            }
            Self::Function { args, .. } => {
                for expression in args {
                    if !visit(expression)? {
                        return Ok(false);
                    }
                }
            }
            Self::FunctionGroup { functions, .. } => {
                for expression in functions.iter().flat_map(|function| &function.args) {
                    if !visit(expression)? {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    /// Append scalar inputs owned by this source, including JOIN predicates; nested query bodies retain their own expression scope.
    pub fn push_expressions<'a>(&'a self, output: &mut Vec<&'a ScalarExpr>) {
        self.try_visit_expressions(&mut |expression| {
            output.push(expression);
            Ok::<_, std::convert::Infallible>(true)
        })
        .unwrap_or_else(|never| match never {});
    }

    pub fn collect_tables(&self, output: &mut Vec<(String, Option<String>)>) {
        match self {
            crate::plan::SourcePlan::RetainedRows(_) => {}
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
    /// Normalize predecessor range nodes while retaining bindings and assigning independent subquery slots to repeated operands.
    pub fn upgrade_legacy_range_predicates(&mut self) -> Result<bool, crate::SQLError> {
        crate::type_resolution::between::restore_expression(self)
    }

    #[must_use]
    pub fn lower(expression: Expr) -> Self {
        Self::lower_with(expression, &NoRegisteredAggregates)
    }

    pub fn lower_with(expression: Expr, aggregates: &dyn AggregateClassifier) -> Self {
        let mut subqueries = Vec::new();
        let scalar = lower_scalar_expression(expression, aggregates, &mut subqueries);
        Self { scalar, subqueries }
    }
}
