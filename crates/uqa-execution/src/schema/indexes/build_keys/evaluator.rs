//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index build keys are already evaluated; their sort still borrows the active value catalog.

use crate::{ExecResult, ExpressionEvaluator, ScalarExpr};
use uqa_core::Value;
use uqa_sql::expr::{RowLookup, SQLValueCatalog};

pub(super) struct IndexKeyEvaluator<'a>(pub Option<&'a (dyn SQLValueCatalog + Send + Sync)>);

impl ExpressionEvaluator for IndexKeyEvaluator<'_> {
    fn evaluate(&self, expression: &ScalarExpr, row: &dyn RowLookup) -> ExecResult<Value> {
        Ok(crate::eval_scalar(
            expression,
            &crate::ScalarEvalContext::from_row_lookup(row, &[]),
        )?)
    }

    fn enum_labels(&self) -> Option<&dyn SQLValueCatalog> {
        self.0.map(|catalog| catalog as &dyn SQLValueCatalog)
    }
}
