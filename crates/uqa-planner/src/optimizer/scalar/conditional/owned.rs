//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Conditional selection retains child owners through lazy rewriting and parent reuse.

use super::{
    literal_value_with_control, OptimizerConfig, OwnedScalarExpr, ProductionControl, SQLError,
    ScalarExpr, Value,
};
use uqa_sql::{ast::BinaryOp, expr::eval_value_comparison_truth_with_engine};

fn null(control: &ProductionControl<'_>) -> Result<OwnedScalarExpr, SQLError> {
    Ok(OwnedScalarExpr::literal(
        control.finish(Value::Null, control.empty_reservation())?,
        control,
    )?)
}

fn case_match(
    expression: &ScalarExpr,
    index: usize,
    control: &ProductionControl<'_>,
) -> Result<Option<bool>, SQLError> {
    let ScalarExpr::Case { base, when, .. } = expression else {
        unreachable!("CASE selection")
    };
    Ok(
        match (
            base.as_deref(),
            literal_value_with_control(&when[index].0, control)?.as_deref(),
        ) {
            (None, Some(Value::Bool(value))) => Some(*value),
            (None, Some(Value::Null)) => Some(false),
            (Some(base), Some(condition)) => literal_value_with_control(base, control)?
                .as_deref()
                .map(|base| {
                    eval_value_comparison_truth_with_engine(
                        BinaryOp::Equal,
                        base,
                        condition,
                        control,
                        None,
                        None,
                    )
                })
                .transpose()?
                .map(|value| value == Some(true)),
            _ => None,
        },
    )
}

pub(in crate::optimizer::scalar) fn optimize_case(
    mut expression: OwnedScalarExpr,
    config: &OptimizerConfig,
    storage: &ProductionControl<'_>,
    mut rewrite: impl FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
) -> Result<OwnedScalarExpr, SQLError> {
    config.production.check()?;
    let ScalarExpr::Case {
        base,
        when,
        else_branch,
    } = &*expression
    else {
        unreachable!("CASE selection")
    };
    let base_count = usize::from(base.is_some());
    let arm_count = when.len();
    let has_else = else_branch.is_some();
    if base_count != 0 {
        expression = expression.map_child(0, storage, &mut rewrite)?;
    }
    let mut kept = 0;
    for index in 0..arm_count {
        config.production.check()?;
        let condition = base_count + 2 * index;
        let result = condition + 1;
        expression = expression.map_child(condition, storage, &mut rewrite)?;
        let matched = case_match(&expression, index, &config.production)?;
        if config.coerced_conditionals && matched == Some(false) {
            for child in [condition, result] {
                expression = expression.map_child(child, storage, |discarded| {
                    drop(discarded);
                    null(storage)
                })?;
            }
            continue;
        }
        // Unbound false arms retain their result types without evaluating their values.
        if matched != Some(false) {
            expression = expression.map_child(result, storage, &mut rewrite)?;
        }
        if config.coerced_conditionals && matched == Some(true) {
            if kept == 0 {
                return Ok(expression.select_child(result));
            }
            let (parent, selected) = expression.replace_child(result, null(storage)?, storage)?;
            return Ok(parent
                .truncate_case_arms(kept, storage)?
                .set_case_else(selected, storage)?);
        }
        expression = expression
            .swap_children(base_count + 2 * kept, condition, storage)?
            .swap_children(base_count + 2 * kept + 1, result, storage)?;
        kept += 1;
        if !config.coerced_conditionals && matched == Some(true) {
            return Ok(expression);
        }
    }
    expression = expression.truncate_case_arms(kept, storage)?;
    let else_index = base_count + 2 * kept;
    if has_else {
        expression = expression.map_child(else_index, storage, &mut rewrite)?;
    }
    if config.coerced_conditionals && kept == 0 {
        if has_else {
            return Ok(expression.select_child(else_index));
        }
        drop(expression);
        return null(storage);
    }
    Ok(expression)
}

pub(in crate::optimizer::scalar) fn optimize_coalesce(
    mut expression: OwnedScalarExpr,
    config: &OptimizerConfig,
    storage: &ProductionControl<'_>,
    mut rewrite: impl FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
) -> Result<OwnedScalarExpr, SQLError> {
    config.production.check()?;
    let ScalarExpr::Func { args, .. } = &*expression else {
        unreachable!("COALESCE arguments")
    };
    let count = args.len();
    for index in 0..count {
        config.production.check()?;
        let mut stops = false;
        expression = expression.map_child(index, storage, |argument| {
            let argument = rewrite(argument)?;
            stops = literal_value_with_control(&argument, &config.production)?
                .as_deref()
                .is_some_and(|value| !matches!(value, Value::Null));
            Ok::<_, SQLError>(argument)
        })?;
        if stops {
            if config.coerced_conditionals {
                expression = expression.truncate_call_arguments(index + 1, storage)?;
            }
            break;
        }
    }
    Ok(expression)
}

#[cfg(test)]
mod tests;
