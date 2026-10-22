//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve consumed values against their lexical query and preceding-source scopes.

use uqa_sql::plan::AnalyzedSource;
use uqa_sql::{RowSchema, ScalarExpr};

use super::{Demand, QueryPlan, RelationalPlan, SourcePlan};

pub(super) struct References<'a> {
    target: &'a RowSchema,
    demand: &'a mut Demand,
    scopes: Vec<RowSchema>,
}

impl<'a> References<'a> {
    pub(super) fn new(target: &'a RowSchema, demand: &'a mut Demand) -> Self {
        Self {
            target,
            demand,
            scopes: Vec::new(),
        }
    }

    pub(super) fn expression(&mut self, expression: &ScalarExpr, subqueries: &[QueryPlan]) {
        super::super::output_demand::visit(expression, &mut |part| {
            let index = match part {
                ScalarExpr::ScalarSubquery(index)
                | ScalarExpr::Exists {
                    subquery: index, ..
                }
                | ScalarExpr::InSubquery {
                    subquery: index, ..
                } => Some(*index),
                _ => None,
            };
            if let Some(index) = index {
                if let Some(query) = subqueries.get(index) {
                    self.query(query);
                }
            } else if !self
                .scopes
                .iter()
                .rev()
                .any(|schema| locally_resolved(part, schema))
            {
                retain(part, self.target, self.demand);
            }
        });
    }

    fn query(&mut self, query: &QueryPlan) {
        for cte in &query.ctes {
            if let Some(query) = cte.body.query() {
                self.query(query);
            } else {
                self.demand.extend(0..self.target.physical_width());
            }
        }
        match &query.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(input) = &block.from {
                    self.source(
                        input,
                        query.analyzed_source.as_deref(),
                        &block.subqueries,
                        &[],
                        false,
                    );
                }
                let length = self.scopes.len();
                self.scopes.push(
                    query
                        .analyzed_source
                        .as_ref()
                        .map(|source| source.schema.clone())
                        .unwrap_or_default(),
                );
                for expr in query_inputs(block) {
                    self.expression(expr, &block.subqueries);
                }
                self.scopes.truncate(length);
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
                self.query(left);
                self.query(right);
                let length = self.scopes.len();
                self.scopes
                    .push(query.analyzed_output.clone().unwrap_or_default());
                for expr in order_by
                    .iter()
                    .map(|order| &order.expr)
                    .chain(limit.as_deref())
                    .chain(offset.as_deref())
                {
                    self.expression(expr, subqueries);
                }
                self.scopes.truncate(length);
            }
            RelationalPlan::Values { rows, subqueries } => {
                for expr in rows.iter().flatten() {
                    self.expression(expr, subqueries);
                }
            }
        }
    }

    pub(super) fn source(
        &mut self,
        source: &SourcePlan,
        analysis: Option<&AnalyzedSource>,
        subqueries: &[QueryPlan],
        preceding: &[RowSchema],
        inherited_lateral: bool,
    ) {
        let length = self.scopes.len();
        if inherited_lateral
            || source.is_explicitly_lateral()
            || matches!(
                source,
                SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
            )
        {
            self.scopes.extend_from_slice(preceding);
        }
        match source {
            SourcePlan::Join {
                left,
                right,
                on,
                lateral,
                ..
            } => {
                if let Some(children) = analysis.and_then(|source| source.children.as_deref()) {
                    self.source(left, Some(&children.0), subqueries, preceding, false);
                    let mut right_preceding = preceding.to_vec();
                    right_preceding.push(children.0.schema.clone());
                    self.source(
                        right,
                        Some(&children.1),
                        subqueries,
                        &right_preceding,
                        *lateral,
                    );
                    if let Some(on) = on {
                        self.scopes.push(children.0.schema.clone());
                        self.scopes.push(children.1.schema.clone());
                        self.expression(on, subqueries);
                    }
                } else {
                    self.demand.extend(0..self.target.physical_width());
                }
            }
            SourcePlan::Subquery { body, .. }
            | SourcePlan::Table {
                executable_query: Some(body),
                ..
            } => self.query(body),
            SourcePlan::Values { rows, .. } => {
                for expr in rows.iter().flatten() {
                    self.expression(expr, subqueries);
                }
            }
            SourcePlan::Function { args, .. } => {
                for expr in args {
                    self.expression(expr, subqueries);
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for expr in functions.iter().flat_map(|function| &function.args) {
                    self.expression(expr, subqueries);
                }
            }
            SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } => {}
        }
        self.scopes.truncate(length);
    }
}

fn locally_resolved(expression: &ScalarExpr, schema: &RowSchema) -> bool {
    match expression {
        ScalarExpr::Column(name) => {
            schema.has_unqualified_column(name) || schema.has_qualifier(name)
        }
        ScalarExpr::QualifiedColumn { qualifier, .. } => schema.has_qualifier(qualifier),
        ScalarExpr::QualifiedStar(qualifier) => schema.has_qualifier(qualifier),
        ScalarExpr::InternalColumn(column) => schema.internal_slot(*column).is_some(),
        ScalarExpr::Star | ScalarExpr::Position(_) => true,
        _ => false,
    }
}

fn retain(expression: &ScalarExpr, schema: &RowSchema, demand: &mut Demand) {
    let slot = match expression {
        ScalarExpr::Column(name) => {
            if let Some(slot) = schema.column_slot(name) {
                Some(slot)
            } else {
                demand.extend(
                    schema
                        .qualified_star_position_layout(name)
                        .into_iter()
                        .map(|(_, _, slot, _)| slot),
                );
                None
            }
        }
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            schema.qualified_slot(qualifier, column)
        }
        ScalarExpr::InternalColumn(column) => schema.internal_slot(*column),
        ScalarExpr::QualifiedStar(qualifier) => {
            demand.extend(
                schema
                    .qualified_star_position_layout(qualifier)
                    .into_iter()
                    .map(|(_, _, slot, _)| slot),
            );
            None
        }
        ScalarExpr::Star => {
            demand.extend(
                (0..schema.len())
                    .filter(|position| schema.wildcard_position_visible(*position))
                    .filter_map(|position| schema.slot(position)),
            );
            None
        }
        ScalarExpr::Position(position) => schema.slot(*position),
        _ => None,
    };
    demand.extend(slot);
}

/// SQL92 ORDER BY and DISTINCT ON names and positions observe an output already included in projection demand; expressions inside those clauses still resolve against the input namespace.
pub(super) fn query_inputs(
    block: &uqa_sql::plan::QueryBlockPlan,
) -> impl Iterator<Item = &ScalarExpr> {
    block.query_expressions().into_iter().filter(|expression| {
        let ordering = block
            .order_by
            .iter()
            .map(|order| &order.expr)
            .chain(&block.distinct_on)
            .any(|order| std::ptr::eq(order, *expression));
        !ordering
            || !match expression {
                ScalarExpr::Position(_) | ScalarExpr::Literal(uqa_core::Value::Int(_)) => true,
                ScalarExpr::Column(name) => block.projections.iter().any(|projection| {
                    uqa_sql::semantics::projection_label_at(projection) == name.as_str()
                }),
                _ => false,
            }
    })
}
