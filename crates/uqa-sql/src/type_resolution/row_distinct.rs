//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve the field comparisons selected from ROW DISTINCT syntax in stored expressions. Removed constructor casts no longer create dependencies; each retained field keeps its own coercions and nested record operator.

use crate::ast::{Expr, FunctionBinding, FunctionDispatch, FunctionOrderSyntax};
use crate::{SQLError, ScalarExpr};
use uqa_core::Value;

fn distinct_arguments(
    expression: &ScalarExpr,
    dispatch: FunctionDispatch,
) -> Option<&[ScalarExpr]> {
    match expression {
        ScalarExpr::Func {
            binding: Some(binding),
            args,
            ..
        } if binding.dispatch == Some(dispatch) => Some(args),
        _ => None,
    }
}

fn written_fields(expression: &ScalarExpr) -> Option<&[ScalarExpr]> {
    match expression {
        ScalarExpr::Row(items) | ScalarExpr::CompositeRow { items, .. } => Some(items),
        ScalarExpr::Cast {
            expr,
            implicit: false,
            ..
        } => written_fields(expr),
        _ => None,
    }
}

/// Recognize a selected expansion before pairing the written and analyzed trees. An unchanged constructor comparison, including a domain cast, retains its original shape.
pub(crate) fn expanded(original: &ScalarExpr, bound: &ScalarExpr) -> bool {
    let Some([left, right]) = distinct_arguments(original, FunctionDispatch::IsDistinct) else {
        return false;
    };
    let (Some(left), Some(right)) = (written_fields(left), written_fields(right)) else {
        return false;
    };
    if left.len() != right.len() {
        return false;
    }
    match bound {
        ScalarExpr::Literal(Value::Bool(false)) => left.is_empty(),
        ScalarExpr::Or(items) => {
            left.len() > 1
                && items.len() == left.len()
                && items.iter().all(|item| {
                    distinct_arguments(item, FunctionDispatch::ValueIsDistinct).is_some()
                })
        }
        _ if left.len() == 1 => {
            distinct_arguments(bound, FunctionDispatch::ValueIsDistinct).is_some()
        }
        _ => false,
    }
}

macro_rules! syntax {
    ($rewrite:ident, $build:ident, $ty:ident) => {
        pub(crate) fn $build(left: Vec<$ty>, right: Vec<$ty>) -> $ty {
            let mut comparisons = left
                .into_iter()
                .zip(right)
                .map(|(left, right)| {
                    let binding = FunctionBinding::dispatched(FunctionDispatch::ValueIsDistinct);
                    $ty::Func {
                        name: binding.name.clone(),
                        binding: Some(binding),
                        args: vec![left, right],
                        distinct: false,
                        order_syntax: FunctionOrderSyntax::Ordinary,
                        order_by: Vec::new(),
                        filter: None,
                    }
                })
                .collect::<Vec<_>>();
            match comparisons.len() {
                0 => $ty::Literal(Value::Bool(false)),
                1 => comparisons.pop().expect("one field comparison"),
                _ => $ty::Or(comparisons),
            }
        }

        pub(crate) fn $rewrite(expression: &mut $ty) -> Result<(), SQLError> {
            fn fields(expression: $ty) -> Result<Vec<$ty>, SQLError> {
                match expression {
                    $ty::Row(items) | $ty::CompositeRow { items, .. } => Ok(items),
                    $ty::Cast {
                        expr,
                        implicit: false,
                        ..
                    } => fields(*expr),
                    _ => Err(SQLError::Internal(
                        "selected ROW comparison has no constructor".into(),
                    )),
                }
            }
            let $ty::Func {
                binding: Some(binding),
                args,
                ..
            } = expression
            else {
                return Err(SQLError::Internal(
                    "selected ROW comparison has no call".into(),
                ));
            };
            if binding.dispatch != Some(FunctionDispatch::IsDistinct) || args.len() != 2 {
                return Err(SQLError::Internal(
                    "selected ROW comparison has invalid operands".into(),
                ));
            }
            let right = fields(args.pop().expect("right operand"))?;
            let left = fields(args.pop().expect("left operand"))?;
            if left.len() != right.len() {
                return Err(SQLError::Internal(
                    "selected ROW comparison changed width".into(),
                ));
            }
            *expression = $build(left, right);
            Ok(())
        }
    };
}

syntax!(rewrite_scalar, scalar_fields, ScalarExpr);
syntax!(rewrite_ast, ast_fields, Expr);
