//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{numeric_operator_precedence, Deparser, QueryPlan, SQLError, ScalarExpr, Scope};

impl Deparser<'_> {
    pub(super) fn distinct_operator(
        &self,
        args: &[ScalarExpr],
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let [left, right] = args else {
            return Err(SQLError::Internal("invalid distinct operands".into()));
        };
        Ok(self.parenthesize(format!(
            "{} IS DISTINCT FROM {}",
            self.operand(left, 80, false, scope, subqueries)?,
            self.operand(right, 80, false, scope, subqueries)?
        )))
    }

    pub(super) fn json_path_call(
        &self,
        name: &str,
        args: &[ScalarExpr],
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let [input, path] = args else {
            return Err(SQLError::Internal(
                "JSON path call requires two arguments".into(),
            ));
        };
        Ok(format!(
            "{name}({}, VARIADIC {})",
            self.expression(input, scope, subqueries)?,
            self.expression(path, scope, subqueries)?
        ))
    }

    pub(super) fn numeric_operator(
        &self,
        operator: uqa_sql::ast::NumericOperator,
        args: &[ScalarExpr],
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let priority = numeric_operator_precedence(operator);
        let arithmetic = priority != 0;
        // Generic numeric operators keep nested operator expressions parenthesized;
        // casts and self-delimiting expressions retain their existing syntax.
        let priority = if arithmetic { priority } else { 80 };
        let separator = |operand: &str| if operand.starts_with('\n') { "" } else { " " };
        let text = match args {
            [argument] if operator.arity() == 1 => {
                let argument = self.operand(argument, priority, false, scope, subqueries)?;
                format!("{}{}{argument}", operator.symbol(), separator(&argument))
            }
            [left, right] if operator.arity() == 2 => {
                let left = self.operand(left, priority, false, scope, subqueries)?;
                let right = self.operand(right, priority, arithmetic, scope, subqueries)?;
                format!("{left} {}{}{right}", operator.symbol(), separator(&right))
            }
            _ => {
                return Err(SQLError::Internal(
                    "invalid numeric operator operands".into(),
                ))
            }
        };
        Ok(self.parenthesize(text))
    }

    pub(super) fn json_operator(
        &self,
        symbol: &str,
        args: &[ScalarExpr],
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let [left, right] = args else {
            return Err(SQLError::Internal(
                "JSON extraction requires two operands".into(),
            ));
        };
        Ok(self.parenthesize(format!(
            "{} {symbol} {}",
            self.operand(left, 80, false, scope, subqueries)?,
            self.operand(right, 80, false, scope, subqueries)?,
        )))
    }
}
