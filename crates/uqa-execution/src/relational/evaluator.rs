//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression-evaluation and row-predicate seams.

use super::{
    eval_scalar, Arc, ExecResult, ResultRow, SQLParam, ScalarEvalContext, ScalarExpr, Value,
};
use uqa_sql::expr::RowLookup;

pub trait ExpressionEvaluator: Send + Sync {
    fn evaluate(&self, expression: &ScalarExpr, row: &dyn RowLookup) -> ExecResult<Value>;

    fn project_star(&self, row: &dyn RowLookup) -> ExecResult<ResultRow> {
        let mut output = ResultRow::new();
        row.visit_columns(&mut |column, value| {
            output.insert(column.to_string(), value.clone());
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
}
