//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar projection and star expansion.

use super::{
    Batch, DefaultExpressionEvaluator, ExecResult, PhysicalOperator, ResultRow, RowSchema,
    SQLParam, ScalarExpr, SharedExpressionEvaluator,
};

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
        let mut columns = Vec::new();
        for (name, expression) in &projections {
            if matches!(expression, ScalarExpr::Star) {
                for column in child.schema() {
                    if !columns.contains(column) {
                        columns.push(column.clone());
                    }
                }
            } else {
                columns.push(name.clone());
            }
        }
        let schema = RowSchema::new(columns);
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
        let ordering = child
            .output_ordering()
            .iter()
            .take_while(|order| {
                projections.iter().all(|(name, expression)| {
                    name != &order.column
                        || matches!(expression, ScalarExpr::Star)
                        || matches!(expression, ScalarExpr::Column(source) if source == name)
                })
            })
            .cloned()
            .collect();
        let mut cols = child.schema().to_vec();
        for (name, _) in &projections {
            if !cols.contains(name) {
                cols.push(name.clone());
            }
        }
        let schema = RowSchema::new(cols);
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
    fn schema(&self) -> &[String] {
        &self.schema.columns
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
            if self.pass_through {
                let mut computed = Vec::with_capacity(self.projections.len());
                for (name, expr) in &self.projections {
                    if matches!(expr, ScalarExpr::Star) {
                        computed.extend(self.evaluator.project_star(&row)?);
                    } else {
                        computed.push((name.clone(), self.evaluator.evaluate(expr, &row)?));
                    }
                }
                let mut new_row = row;
                new_row.extend(computed);
                out.push(new_row);
                continue;
            }
            let mut new_row = ResultRow::new();
            for (name, expr) in &self.projections {
                if matches!(expr, ScalarExpr::Star) {
                    for (column, value) in self.evaluator.project_star(&row)? {
                        new_row.insert(column, value);
                    }
                } else {
                    let value = self.evaluator.evaluate(expr, &row)?;
                    new_row.insert(name.clone(), value);
                }
            }
            out.push(new_row);
        }
        Ok(Some(Batch::new(self.schema.clone(), out)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

// -------------------------------------------------------------------------
// Sort
// -------------------------------------------------------------------------
