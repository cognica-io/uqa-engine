//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Visit executable expression branches while retaining unreachable arms for result typing.

use crate::{ast::BinaryOp, SQLError, ScalarExpr};
use uqa_core::Value;

pub fn visit(
    expression: &ScalarExpr,
    visitor: &mut impl FnMut(&ScalarExpr) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    expression.try_visit(&mut |part| {
        visitor(part)?;
        match part {
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            } => {
                if let Some(base) = base {
                    visit(base, visitor)?;
                }
                for (condition, result) in when {
                    visit(condition, visitor)?;
                    let matched = match (base.as_deref(), literal(condition)) {
                        (None, Some(Value::Bool(value))) => Some(*value),
                        (None, Some(Value::Null)) => Some(false),
                        (Some(base), Some(condition)) => literal(base)
                            .map(|base| {
                                crate::expr::eval_binary_values(BinaryOp::Equal, base, condition)
                            })
                            .transpose()?
                            .map(|value| matches!(value, Value::Bool(true))),
                        _ => None,
                    };
                    if matched != Some(false) {
                        visit(result, visitor)?;
                    }
                    if matched == Some(true) {
                        return Ok(false);
                    }
                }
                if let Some(otherwise) = else_branch {
                    visit(otherwise, visitor)?;
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
                    visit(arg, visitor)?;
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
