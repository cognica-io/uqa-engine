//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression-evaluation and row-predicate seams.

use super::{
    eval_scalar, Arc, ExecResult, ResultRow, SQLParam, ScalarEvalContext, ScalarExpr, Value,
};

pub trait ExpressionEvaluator: Send + Sync {
    fn evaluate(&self, expression: &ScalarExpr, row: &ResultRow) -> ExecResult<Value>;

    fn project_star(&self, row: &ResultRow) -> ExecResult<ResultRow> {
        Ok(row.clone())
    }
}

pub type SharedExpressionEvaluator<'a> = Arc<dyn ExpressionEvaluator + 'a>;

pub trait RowPredicate: Send + Sync {
    fn keep(&self, row: &ResultRow) -> ExecResult<bool>;
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
    fn evaluate(&self, expression: &ScalarExpr, row: &ResultRow) -> ExecResult<Value> {
        let context = ScalarEvalContext::new(Some(row), &self.params);
        Ok(eval_scalar(expression, &context)?)
    }
}
