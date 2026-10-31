//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Visit executable expression branches while retaining unreachable arms for result typing.

use crate::{ast::BinaryOp, SQLError, ScalarExpr};
use uqa_core::{memory::ProductionControl, Value};

pub fn visit(
    expression: &ScalarExpr,
    visitor: &mut impl FnMut(&ScalarExpr) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    visit_with_control(expression, visitor, &ProductionControl::uncontrolled())
}

/// Visit only executable branches with the original production allowance and both cancellation scopes. Literal CASE comparisons use the same controlled value operations as scalar evaluation.
pub fn visit_with_control(
    expression: &ScalarExpr,
    visitor: &mut impl FnMut(&ScalarExpr) -> Result<(), SQLError>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    expression.try_visit(&mut |part| {
        control.check()?;
        visitor(part)?;
        control.check()?;
        match part {
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            } => {
                if let Some(base) = base {
                    visit_with_control(base, visitor, control)?;
                }
                for (condition, result) in when {
                    visit_with_control(condition, visitor, control)?;
                    let matched = match (base.as_deref(), literal(condition)) {
                        (None, Some(Value::Bool(value))) => Some(*value),
                        (None, Some(Value::Null)) => Some(false),
                        (Some(base), Some(condition)) => literal(base)
                            .map(|base| {
                                crate::expr::eval_binary_values_with_control(
                                    BinaryOp::Equal,
                                    base,
                                    condition,
                                    control,
                                )
                            })
                            .transpose()?
                            .map(|value| matches!(*value, Value::Bool(true))),
                        _ => None,
                    };
                    if matched != Some(false) {
                        visit_with_control(result, visitor, control)?;
                    }
                    if matched == Some(true) {
                        return Ok(false);
                    }
                }
                if let Some(otherwise) = else_branch {
                    visit_with_control(otherwise, visitor, control)?;
                }
                Ok(false)
            }
            ScalarExpr::Func {
                name,
                binding,
                args,
                ..
            } if name.eq_ignore_ascii_case("coalesce")
                && binding.as_ref().is_none_or(|b| b.builtin) =>
            {
                for arg in args {
                    visit_with_control(arg, visitor, control)?;
                    if literal(arg).is_some_and(|value| !matches!(value, Value::Null)) {
                        break;
                    }
                }
                Ok(false)
            }
            _ => Ok(true),
        }
    })
}

fn literal(expression: &ScalarExpr) -> Option<&Value> {
    match expression {
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
