//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSON extraction resolves both operands before runtime strictness can skip them.

use super::ownership::AnalysisType;
use super::{ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr};
use uqa_core::Value;

impl Preparation<'_> {
    pub(super) fn json_extraction(
        &mut self,
        symbol: &str,
        arguments: &[ScalarExpr],
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let [left, right] = arguments else {
            return Err(SQLError::Internal(
                "JSON extraction requires two operands".into(),
            ));
        };
        let mut values = [
            self.expression(left, input, subqueries)?,
            self.expression(right, input, subqueries)?,
        ];
        if values.iter().any(ExpressionType::is_deferred) {
            return Ok(ExpressionType::combined(None, &values));
        }
        let owner = self.scope.control().capture();
        let control = owner.control();
        let selected = crate::type_resolution::named_binary_operator_types_with_control(
            symbol,
            values[0].ty.as_deref(),
            values[1].ty.as_deref(),
            &control,
        )?;
        let [left, right, result] = [&selected[0], &selected[1], &selected[2]]
            .map(|ty| AnalysisType::from_produced(ty.clone_with_control(&control)?, &control));
        let (left, right, result) = (left?, right?, result?);
        for ((argument, value), target) in arguments.iter().zip(&mut values).zip([left, right]) {
            let needs_cast = value.ty.as_deref().is_some_and(|source| *source != *target)
                || matches!(argument, ScalarExpr::Literal(Value::Null));
            self.parameters.coerce_unknown(value, &target)?;
            if needs_cast {
                self.parameters.retain_input_coercion(argument, target)?;
            }
        }
        Ok(ExpressionType::combined_owned(Some(result), &values))
    }
}
