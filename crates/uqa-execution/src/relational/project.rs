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
        }
    }

    pub fn appending_with_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        projections: Vec<(String, ScalarExpr)>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
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
        }
    }
}

impl PhysicalOperator for Project<'_> {
    fn schema(&self) -> &[String] {
        &self.schema.columns
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
            let mut new_row: ResultRow = if self.pass_through {
                row.clone()
            } else {
                ResultRow::new()
            };
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
