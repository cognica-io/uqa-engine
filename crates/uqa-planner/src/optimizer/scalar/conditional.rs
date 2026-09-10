//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered simplification of conditional expressions.

use std::collections::VecDeque;

use uqa_sql::ast::BinaryOp;
use uqa_sql::expr::eval_binary_values;

use super::constants::literal_value;
use super::{optimize_optional, optimize_scalar, OptimizerConfig, SQLError, ScalarExpr, Value};

pub(super) fn optimize_boolean(
    items: Vec<ScalarExpr>,
    and: bool,
    config: &OptimizerConfig,
) -> Result<ScalarExpr, SQLError> {
    let mut pending = VecDeque::from(items);
    let mut kept = Vec::new();
    let mut have_null = false;
    while let Some(item) = pending.pop_front() {
        match item {
            ScalarExpr::And(items) if and => {
                for item in items.into_iter().rev() {
                    pending.push_front(item);
                }
                continue;
            }
            ScalarExpr::Or(items) if !and => {
                for item in items.into_iter().rev() {
                    pending.push_front(item);
                }
                continue;
            }
            _ => {}
        }
        let item = optimize_scalar(item, config)?;
        match literal_value(&item) {
            Some(Value::Bool(value)) if *value != and => {
                if config.enable_boolean_simplify {
                    return Ok(ScalarExpr::Literal(Value::Bool(*value)));
                }
                kept.push(item);
                kept.extend(pending);
                break;
            }
            Some(Value::Bool(_)) if config.enable_boolean_simplify => {}
            Some(Value::Null) if config.enable_boolean_simplify => have_null = true,
            _ => kept.push(item),
        }
    }
    if have_null {
        kept.push(ScalarExpr::TypedLiteral {
            value: Value::Null,
            ty: "boolean".into(),
            bound_type: None,
            parameter_index: None,
        });
    }
    if config.enable_boolean_simplify {
        match kept.len() {
            0 => return Ok(ScalarExpr::Literal(Value::Bool(and))),
            1 => return Ok(kept.remove(0)),
            _ => {}
        }
    }
    Ok(if and {
        ScalarExpr::And(kept)
    } else {
        ScalarExpr::Or(kept)
    })
}

pub(super) fn optimize_case(
    base: Option<Box<ScalarExpr>>,
    when: Vec<(ScalarExpr, ScalarExpr)>,
    else_branch: Option<Box<ScalarExpr>>,
    config: &OptimizerConfig,
) -> Result<ScalarExpr, SQLError> {
    let base = optimize_optional(base, config)?;
    let mut kept = Vec::with_capacity(when.len());
    let mut remaining = when.into_iter();
    while let Some((condition, result)) = remaining.next() {
        let condition = optimize_scalar(condition, config)?;
        let matched = match (base.as_deref(), literal_value(&condition)) {
            (None, Some(Value::Bool(value))) => Some(*value),
            (None, Some(Value::Null)) => Some(false),
            (Some(base), Some(condition)) => literal_value(base)
                .map(|base| eval_binary_values(BinaryOp::Equal, base, condition))
                .transpose()?
                .map(|value| matches!(value, Value::Bool(true))),
            _ => None,
        };
        // Retain unreachable arms for schema binding: their types still
        // participate in CASE's common result type, but their values do not.
        let result = if matched == Some(false) {
            result
        } else {
            optimize_scalar(result, config)?
        };
        kept.push((condition, result));
        if matched == Some(true) {
            kept.extend(remaining);
            return Ok(ScalarExpr::Case {
                base,
                when: kept,
                else_branch,
            });
        }
    }
    Ok(ScalarExpr::Case {
        base,
        when: kept,
        else_branch: optimize_optional(else_branch, config)?,
    })
}

pub(super) fn optimize_coalesce(
    args: Vec<ScalarExpr>,
    config: &OptimizerConfig,
) -> Result<Vec<ScalarExpr>, SQLError> {
    let mut kept = Vec::with_capacity(args.len());
    let mut remaining = args.into_iter();
    while let Some(argument) = remaining.next() {
        let argument = optimize_scalar(argument, config)?;
        let stops = literal_value(&argument).is_some_and(|value| !matches!(value, Value::Null));
        kept.push(argument);
        if stops {
            kept.extend(remaining);
            break;
        }
    }
    Ok(kept)
}
