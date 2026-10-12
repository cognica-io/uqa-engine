//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{numeric_operator_precedence, Deparser, QueryPlan, SQLError, ScalarExpr, Scope};

impl Deparser<'_> {
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
        let text = match args {
            [argument] if operator.arity() == 1 => format!(
                "{} {}",
                operator.symbol(),
                self.operand(argument, priority, false, scope, subqueries)?
            ),
            [left, right] if operator.arity() == 2 => format!(
                "{} {} {}",
                self.operand(left, priority, false, scope, subqueries)?,
                operator.symbol(),
                self.operand(right, priority, true, scope, subqueries)?
            ),
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
