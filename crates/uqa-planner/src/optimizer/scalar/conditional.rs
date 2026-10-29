//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered simplification of conditional expressions.

use std::collections::VecDeque;

use uqa_sql::ast::BinaryOp;
use uqa_sql::expr::eval_value_comparison_truth_with_engine;

use super::constants::literal_value_with_control;
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
        match literal_value_with_control(&item, &config.production)?.as_deref() {
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
            collation_oid: None,
            composite_source: None,
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
    mut when: Vec<(ScalarExpr, ScalarExpr)>,
    else_branch: Option<Box<ScalarExpr>>,
    config: &OptimizerConfig,
) -> Result<ScalarExpr, SQLError> {
    let base = optimize_optional(base, config)?;
    let mut kept = 0;
    for index in 0..when.len() {
        let (condition, result) = std::mem::replace(
            &mut when[index],
            (
                ScalarExpr::Literal(Value::Null),
                ScalarExpr::Literal(Value::Null),
            ),
        );
        let condition = optimize_scalar(condition, config)?;
        let matched = match (
            base.as_deref(),
            literal_value_with_control(&condition, &config.production)?.as_deref(),
        ) {
            (None, Some(Value::Bool(value))) => Some(*value),
            (None, Some(Value::Null)) => Some(false),
            (Some(base), Some(condition)) => literal_value_with_control(base, &config.production)?
                .as_deref()
                .map(|base| {
                    eval_value_comparison_truth_with_engine(
                        BinaryOp::Equal,
                        base,
                        condition,
                        &config.production,
                        None,
                        None,
                    )
                })
                .transpose()?
                .map(|value| value == Some(true)),
            _ => None,
        };
        if config.coerced_conditionals {
            if matched == Some(false) {
                continue;
            }
            let result = optimize_scalar(result, config)?;
            if matched == Some(true) {
                when.truncate(kept);
                return Ok(if kept == 0 {
                    result
                } else {
                    let else_branch = match else_branch {
                        Some(mut branch) => {
                            *branch = result;
                            branch
                        }
                        None => Box::new(result),
                    };
                    ScalarExpr::Case {
                        base,
                        when,
                        else_branch: Some(else_branch),
                    }
                });
            }
            when[kept] = (condition, result);
            kept += 1;
            continue;
        }
        // Retain unreachable arms for schema binding: their types still participate in CASE's common result type, but their values do not.
        let result = if matched == Some(false) {
            result
        } else {
            optimize_scalar(result, config)?
        };
        when[kept] = (condition, result);
        kept += 1;
        if matched == Some(true) {
            return Ok(ScalarExpr::Case {
                base,
                when,
                else_branch,
            });
        }
    }
    when.truncate(kept);
    if config.coerced_conditionals && kept == 0 {
        return Ok(optimize_optional(else_branch, config)?
            .map_or(ScalarExpr::Literal(Value::Null), |value| *value));
    }
    Ok(ScalarExpr::Case {
        base,
        when,
        else_branch: optimize_optional(else_branch, config)?,
    })
}

pub(super) fn optimize_coalesce(
    mut args: Vec<ScalarExpr>,
    config: &OptimizerConfig,
) -> Result<Vec<ScalarExpr>, SQLError> {
    for index in 0..args.len() {
        let argument = std::mem::replace(&mut args[index], ScalarExpr::Literal(Value::Null));
        let argument = optimize_scalar(argument, config)?;
        let stops = literal_value_with_control(&argument, &config.production)?
            .as_deref()
            .is_some_and(|value| !matches!(value, Value::Null));
        args[index] = argument;
        if stops {
            if config.coerced_conditionals {
                args.truncate(index + 1);
            }
            break;
        }
    }
    Ok(args)
}
