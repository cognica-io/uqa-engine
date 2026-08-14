//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression-evaluation and row-predicate seams.

use super::{
    eval_scalar, Arc, ExecResult, RowSchema, SQLParam, ScalarEvalContext, ScalarExpr, Value,
};
use uqa_sql::ast::ColumnType;
use uqa_sql::expr::RowLookup;
use uqa_sql::SQLError;

pub trait ExpressionEvaluator: Send + Sync {
    fn evaluate(&self, expression: &ScalarExpr, row: &dyn RowLookup) -> ExecResult<Value>;

    /// Bound SQL parameters used by static type resolution. Implementations
    /// that do not evaluate parameters may keep the empty default.
    fn parameters(&self) -> &[SQLParam] {
        &[]
    }

    fn expression_type(
        &self,
        expression: &ScalarExpr,
        schema: &RowSchema,
    ) -> Result<Option<ColumnType>, SQLError> {
        crate::scalar_type(expression, schema, self.parameters())
    }

    fn star_column_visible(&self, _column: &str) -> bool {
        true
    }

    fn project_star(&self, row: &dyn RowLookup) -> ExecResult<Vec<(String, Value)>> {
        let mut output = Vec::new();
        row.visit_columns(&mut |column, value| {
            if self.star_column_visible(column) {
                output.push((column.to_string(), value.clone()));
            }
        });
        Ok(output)
    }
}

pub type SharedExpressionEvaluator<'a> = Arc<dyn ExpressionEvaluator + 'a>;

pub trait RowPredicate: Send + Sync {
    fn keep(&self, row: &dyn RowLookup) -> ExecResult<bool>;
}

pub type SharedRowPredicate<'a> = Arc<dyn RowPredicate + 'a>;

pub(super) struct DefaultExpressionEvaluator {
    params: Vec<SQLParam>,
}

impl DefaultExpressionEvaluator {
    pub(super) fn shared(params: Vec<SQLParam>) -> SharedExpressionEvaluator<'static> {
        Arc::new(Self { params })
    }
}

impl ExpressionEvaluator for DefaultExpressionEvaluator {
    fn evaluate(&self, expression: &ScalarExpr, row: &dyn RowLookup) -> ExecResult<Value> {
        let context = ScalarEvalContext::from_row_lookup(row, &self.params);
        Ok(eval_scalar(expression, &context)?)
    }

    fn parameters(&self) -> &[SQLParam] {
        &self.params
    }
}
