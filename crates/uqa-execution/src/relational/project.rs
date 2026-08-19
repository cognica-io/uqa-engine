//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar projection and star expansion.

use uqa_core::Value;

use super::{
    Batch, DefaultExpressionEvaluator, ExecResult, PhysicalOperator, RowSchema, SQLParam,
    ScalarExpr, SharedExpressionEvaluator,
};
use crate::batch::ProjectedSlot;

/// Per-row scalar projection. Each `(alias, expr)` pair is evaluated
/// against the input row and written under `alias` in the output. The
/// child schema is replaced with the output aliases.
pub struct Project<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    computed: Vec<ScalarExpr>,
    evaluator: SharedExpressionEvaluator<'a>,
    schema: RowSchema,
    ordering: Vec<crate::PhysicalOrder>,
}

fn projection_layout(
    input: &RowSchema,
    projections: Vec<(String, ScalarExpr)>,
    evaluator: &SharedExpressionEvaluator<'_>,
    pass_through: bool,
) -> (RowSchema, Vec<ScalarExpr>) {
    let mut projected = Vec::new();
    let mut computed = Vec::new();
    for (name, expression) in projections {
        if let ScalarExpr::QualifiedStar(qualifier) = &expression {
            if pass_through {
                continue;
            }
            for (column, logical, _, ty) in input.qualified_star_position_layout(qualifier) {
                if !evaluator.star_column_visible(&column) {
                    continue;
                }
                let slot = logical
                    .and_then(|logical| input.physical_slot(logical))
                    .or_else(|| {
                        input.physical_slot_for_identity(&crate::ColumnIdentity::qualified(
                            qualifier, &column,
                        ))
                    });
                projected.push((column, ty, ProjectedSlot::Input(slot)));
            }
            continue;
        }
        if matches!(expression, ScalarExpr::Star) {
            if pass_through {
                continue;
            }
            for (logical, column) in input.iter().enumerate() {
                if !evaluator.star_column_visible(column) {
                    continue;
                }
                projected.push((
                    input.public_name(logical).unwrap_or(column).to_string(),
                    input.column_type(logical).cloned(),
                    ProjectedSlot::Input(input.physical_slot(logical)),
                ));
            }
            continue;
        }

        let ty = evaluator.expression_type(&expression, input).ok().flatten();
        if let Some(logical) = crate::order_expression_position(input, &expression) {
            projected.push((name, ty, ProjectedSlot::Input(input.physical_slot(logical))));
        } else {
            projected.push((name, ty, ProjectedSlot::Computed));
            computed.push(expression);
        }
    }
    (
        RowSchema::project_with_sources(input, projected, pass_through),
        computed,
    )
}

impl Project<'static> {
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        projections: Vec<(String, ScalarExpr)>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::with_evaluator(
            child,
            projections,
            DefaultExpressionEvaluator::shared(params),
        )
    }

    /// Variant that keeps every input column in the output and appends
    /// the projections at the end. Used by aggregate / window paths.
    pub fn appending(
        child: Box<dyn PhysicalOperator>,
        projections: Vec<(String, ScalarExpr)>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::appending_with_evaluator(
            child,
            projections,
            DefaultExpressionEvaluator::shared(params),
        )
    }
}

impl<'a> Project<'a> {
    pub fn with_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        projections: Vec<(String, ScalarExpr)>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        let params = evaluator.parameters();
        let projections = projections
            .into_iter()
            .map(|(name, expression)| {
                (
                    name,
                    crate::bind_type_introspection(expression, child.row_schema(), params),
                )
            })
            .collect::<Vec<_>>();
        let (schema, computed) =
            projection_layout(child.row_schema(), projections, &evaluator, false);
        Self {
            child,
            computed,
            evaluator,
            schema,
            ordering: Vec::new(),
        }
    }

    pub fn appending_with_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        projections: Vec<(String, ScalarExpr)>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        let params = evaluator.parameters();
        let projections = projections
            .into_iter()
            .map(|(name, expression)| {
                (
                    name,
                    crate::bind_type_introspection(expression, child.row_schema(), params),
                )
            })
            .collect::<Vec<_>>();
        let ordering = child
            .output_ordering()
            .iter()
            .take_while(|order| {
                projections.iter().all(|(name, expression)| {
                    child
                        .row_schema()
                        .columns()
                        .iter()
                        .position(|column| column == name)
                        != Some(order.position)
                        || matches!(expression, ScalarExpr::Star | ScalarExpr::QualifiedStar(_))
                        || crate::order_expression_position(child.row_schema(), expression)
                            == Some(order.position)
                })
            })
            .cloned()
            .collect();
        let (schema, computed) =
            projection_layout(child.row_schema(), projections, &evaluator, true);
        Self {
            child,
            computed,
            evaluator,
            schema,
            ordering,
        }
    }
}

impl PhysicalOperator for Project<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn output_ordering(&self) -> &[crate::PhysicalOrder] {
        &self.ordering
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(batch) = self.child.next()? else {
            return Ok(None);
        };
        if self.computed.is_empty() {
            return Ok(Some(Batch::from_physical_rows(
                self.schema.clone(),
                batch.rows,
            )));
        }
        let mut out = Vec::with_capacity(batch.rows.len());
        for row in batch.rows {
            let values = self
                .computed
                .iter()
                .map(|expression| {
                    self.evaluator
                        .evaluate_physical(expression, &batch.schema, &row)
                })
                .collect::<ExecResult<Vec<Value>>>()?;
            out.push(row.append_values(values));
        }
        Ok(Some(Batch::from_physical_rows(self.schema.clone(), out)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

// -------------------------------------------------------------------------
// Sort
// -------------------------------------------------------------------------
