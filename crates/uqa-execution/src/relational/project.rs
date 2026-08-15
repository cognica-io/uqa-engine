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
use crate::PhysicalRow;

/// Per-row scalar projection. Each `(alias, expr)` pair is evaluated
/// against the input row and written under `alias` in the output. The
/// child schema is replaced with the output aliases.
pub struct Project<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    projections: Vec<(String, ScalarExpr)>,
    evaluator: SharedExpressionEvaluator<'a>,
    schema: RowSchema,
    /// When `true`, every input column also flows through to the
    /// output (after any alias rewrite). Useful when projections only
    /// derive new columns.
    pass_through: bool,
    ordering: Vec<crate::PhysicalOrder>,
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
        let mut columns = Vec::new();
        let mut types = Vec::new();
        for (name, expression) in &projections {
            if let ScalarExpr::QualifiedStar(qualifier) = expression {
                for (column, _, ty) in child.row_schema().qualified_star_layout(qualifier) {
                    if evaluator.star_column_visible(&column) {
                        columns.push(column);
                        types.push(ty);
                    }
                }
            } else if matches!(expression, ScalarExpr::Star) {
                for (position, column) in child.schema().iter().enumerate() {
                    if evaluator.star_column_visible(column) {
                        columns.push(
                            child
                                .row_schema()
                                .public_name(position)
                                .unwrap_or(column)
                                .to_string(),
                        );
                        types.push(child.row_schema().column_type(position).cloned());
                    }
                }
            } else {
                columns.push(name.clone());
                types.push(
                    evaluator
                        .expression_type(expression, child.row_schema())
                        .ok()
                        .flatten(),
                );
            }
        }
        let schema = RowSchema::with_types(columns, types);
        Self {
            child,
            projections,
            evaluator,
            schema,
            pass_through: false,
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
                    name != &order.column
                        || matches!(expression, ScalarExpr::Star | ScalarExpr::QualifiedStar(_))
                        || matches!(expression, ScalarExpr::Column(source) if source == name)
                })
            })
            .cloned()
            .collect();
        let appended = projections
            .iter()
            .filter(|(_, expression)| {
                !matches!(expression, ScalarExpr::Star | ScalarExpr::QualifiedStar(_))
            })
            .map(|(name, expression)| {
                (
                    name.clone(),
                    evaluator
                        .expression_type(expression, child.row_schema())
                        .ok()
                        .flatten(),
                )
            })
            .collect::<Vec<_>>();
        let schema = RowSchema::append_typed(child.row_schema(), &appended);
        Self {
            child,
            projections,
            evaluator,
            schema,
            pass_through: true,
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
        let mut out = Vec::with_capacity(batch.rows.len());
        for row in batch.rows {
            let view = batch.schema.view(&row);
            if self.pass_through {
                let mut computed = Vec::with_capacity(self.projections.len());
                for (_, expr) in &self.projections {
                    if !matches!(expr, ScalarExpr::Star | ScalarExpr::QualifiedStar(_)) {
                        computed.push(self.evaluator.evaluate_physical(
                            expr,
                            &batch.schema,
                            &row,
                        )?);
                    }
                }
                out.push(row.append_values(computed));
                continue;
            }
            if self.projections.iter().any(|(_, expression)| {
                matches!(expression, ScalarExpr::Star | ScalarExpr::QualifiedStar(_))
            }) {
                let mut values = Vec::with_capacity(self.schema.len());
                for (name, expr) in &self.projections {
                    if let ScalarExpr::QualifiedStar(qualifier) = expr {
                        for (column, slot, _) in batch.schema.qualified_star_layout(qualifier) {
                            if self.evaluator.star_column_visible(&column) {
                                values.push(row.value(slot).cloned().unwrap_or(Value::Null));
                            }
                        }
                    } else if matches!(expr, ScalarExpr::Star) {
                        for (position, column) in batch.schema.iter().enumerate() {
                            if self.evaluator.star_column_visible(column) {
                                values
                                    .push(view.value_at(position).cloned().unwrap_or(Value::Null));
                            }
                        }
                    } else {
                        let _ = name;
                        values.push(
                            self.evaluator
                                .evaluate_physical(expr, &batch.schema, &row)?,
                        );
                    }
                }
                out.push(PhysicalRow::from_values(values));
                continue;
            }
            let values = self
                .projections
                .iter()
                .map(|(_, expression)| {
                    self.evaluator
                        .evaluate_physical(expression, &batch.schema, &row)
                })
                .collect::<ExecResult<Vec<_>>>()?;
            out.push(PhysicalRow::from_values(values));
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
