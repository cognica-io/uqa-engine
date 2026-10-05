//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain added-column defaults in the same per-row evaluation queue as type transforms.

use super::super::{backfill::ColumnBackfillContext, rows::RewriteDeferral};
use std::cell::RefCell;
use uqa_core::Value;
use uqa_sql::{assignment::columns::coerce_to_column_type, ast::Expr, SQLError};

pub enum AddedValue {
    Constant(Value),
    Expression(Expr),
}

pub struct AddedColumnValue {
    pub table: String,
    pub column: String,
    pub value: AddedValue,
}

#[derive(Default)]
pub struct AddedColumnRows {
    pub deferral: RewriteDeferral,
    pending: RefCell<Vec<AddedColumnValue>>,
}

impl AddedColumnRows {
    pub fn retain(
        &self,
        context: &ColumnBackfillContext<'_>,
        table: &str,
        column: &str,
        expression: Option<Expr>,
    ) -> Result<(), SQLError> {
        let value = match expression {
            Some(expression)
                if uqa_sql::semantics::volatility::expr_contains_volatile_function(
                    context.volatility,
                    &uqa_sql::plan::ExpressionPlan::lower(expression.clone()).scalar,
                ) =>
            {
                AddedValue::Expression(expression)
            }
            expression => {
                let value = expression.as_ref().map_or(Ok(Value::Null), |expression| {
                    context.rewrite.expressions.evaluate_bound(expression, &[])
                })?;
                AddedValue::Constant(coerce_to_column_type(
                    context.rewrite.types,
                    context.rewrite.columns,
                    table,
                    column,
                    value,
                )?)
            }
        };
        self.pending.borrow_mut().push(AddedColumnValue {
            table: table.to_string(),
            column: column.to_string(),
            value,
        });
        Ok(())
    }

    pub fn take(&self) -> Vec<AddedColumnValue> {
        std::mem::take(&mut *self.pending.borrow_mut())
    }
}

impl AddedColumnValue {
    pub fn evaluate(&self, context: &ColumnBackfillContext<'_>) -> Result<Value, SQLError> {
        match &self.value {
            AddedValue::Constant(value) => Ok(value.clone()),
            AddedValue::Expression(expression) => coerce_to_column_type(
                context.rewrite.types,
                context.rewrite.columns,
                &self.table,
                &self.column,
                context
                    .rewrite
                    .expressions
                    .evaluate_bound(expression, &[])?,
            ),
        }
    }
}
