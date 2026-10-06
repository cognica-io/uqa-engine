//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Symbolic invocation argument materialization and parameter substitution.

use super::RoutineInliningContext;
use crate::{
    ast::{CreateFunction, RoutineInvocationBinding, RoutineVariadicMode},
    plan::ExpressionPlan,
    ColumnType, SQLError, ScalarExpr,
};

pub(super) fn prepare(
    context: &RoutineInliningContext<'_>,
    definition: &CreateFunction,
    invocation: &RoutineInvocationBinding,
    arguments: &[ScalarExpr],
) -> Result<Option<Vec<ScalarExpr>>, SQLError> {
    let arguments = crate::scalar_call_arguments(arguments)?;
    if invocation.argument_positions.len() != arguments.len()
        || invocation.argument_targets.len() != arguments.len()
        || invocation.parameter_types.len() != definition.params.len()
    {
        return Err(SQLError::Internal(
            "inconsistent routine invocation during expansion".into(),
        ));
    }
    let expanded = match invocation.variadic_mode {
        RoutineVariadicMode::Expanded { parameter_index } => Some(parameter_index),
        RoutineVariadicMode::None | RoutineVariadicMode::Explicit { .. } => None,
    };
    let mut slots = vec![None; definition.params.len()];
    let mut values = Vec::new();
    for (index, (argument, position)) in arguments
        .iter()
        .zip(&invocation.argument_positions)
        .enumerate()
    {
        let source = invocation
            .argument_sources
            .get(index)
            .and_then(Option::as_deref)
            .map(|name| context.types.resolve_catalog_column_type_name(name))
            .transpose()?;
        let argument = coerce(
            context,
            argument.value.clone(),
            source.as_ref(),
            &invocation.argument_targets[index],
        )?;
        if Some(*position) == expanded {
            values.push(argument);
        } else {
            let slot = slots.get_mut(*position).ok_or_else(|| {
                SQLError::Internal("routine parameter index is out of range".into())
            })?;
            if slot.replace(argument).is_some() {
                return Err(SQLError::Internal(
                    "duplicate routine argument during expansion".into(),
                ));
            }
        }
    }
    if let Some(position) = expanded {
        let Some(slot) = slots.get_mut(position) else {
            return Err(SQLError::Internal(
                "variadic parameter index is out of range".into(),
            ));
        };
        *slot = Some(ScalarExpr::Cast {
            implicit: true,
            expr: Box::new(ScalarExpr::Array(values)),
            ty: invocation.parameter_types[position].clone(),
        });
    }
    let mut result = Vec::with_capacity(definition.identity_arity());
    for (position, parameter) in definition.params.iter().enumerate() {
        if !super::super::body_parameters::is_sql_body_parameter(parameter) {
            continue;
        }
        let argument = if let Some(argument) = slots[position].take() {
            argument
        } else {
            let Some(default) = &parameter.default else {
                return Err(SQLError::Internal(
                    "required routine argument disappeared during expansion".into(),
                ));
            };
            let plan = ExpressionPlan::lower(default.clone());
            if !plan.subqueries.is_empty() {
                return Ok(None);
            }
            let source = match &parameter.default_type {
                Some(crate::ast::RoutineDefaultType::Concrete(ty)) => Some(ty),
                _ => None,
            };
            coerce(
                context,
                plan.scalar,
                source,
                &invocation.parameter_types[position],
            )?
        };
        result.push(argument);
    }
    Ok(Some(result))
}

fn coerce(
    context: &RoutineInliningContext<'_>,
    expression: ScalarExpr,
    source: Option<&ColumnType>,
    target: &str,
) -> Result<ScalarExpr, SQLError> {
    let target = context.types.resolve_catalog_column_type_name(target)?;
    let actual = match &expression {
        ScalarExpr::TypedLiteral {
            bound_type: Some(ty),
            ..
        } => Some(ty),
        _ => source,
    };
    if matches!(expression, ScalarExpr::Literal(uqa_core::Value::Null))
        && !matches!(target, ColumnType::Domain { .. })
    {
        Ok(ScalarExpr::TypedLiteral {
            value: uqa_core::Value::Null,
            ty: target.catalog_name(),
            bound_type: Some(target),
            parameter_index: None,
        })
    } else if actual == Some(&target) {
        Ok(expression)
    } else {
        Ok(ScalarExpr::Cast {
            implicit: true,
            expr: Box::new(expression),
            ty: target.catalog_name(),
        })
    }
}

pub(super) fn use_counts(expression: &ScalarExpr, count: usize) -> Result<Vec<usize>, SQLError> {
    let mut uses = vec![0; count];
    let mut invalid = false;
    expression.visit(&mut |expression| {
        // Parse analysis expands BETWEEN into two comparisons of its operand.
        // Account for that second use before deciding whether an actual input
        // may be copied; ordinary traversal accounts for the first use.
        if let ScalarExpr::Between { expr, .. } = expression {
            expr.visit(&mut |operand| {
                if let ScalarExpr::Param(index) = operand {
                    if let Some(slot) = index.checked_sub(1).and_then(|index| uses.get_mut(index)) {
                        *slot = 2;
                    } else {
                        invalid = true;
                    }
                }
            });
        }
        if let ScalarExpr::Param(index) = expression {
            if let Some(slot) = index.checked_sub(1).and_then(|index| uses.get_mut(index)) {
                *slot = (*slot + 1).min(2);
            } else {
                invalid = true;
            }
        }
    });
    if invalid {
        Err(SQLError::Internal(
            "SQL body parameter is outside its invocation".into(),
        ))
    } else {
        Ok(uses)
    }
}

pub(super) fn substitute(expression: &mut ScalarExpr, arguments: &[ScalarExpr]) {
    // Post-order replacement never visits an inserted caller parameter, which
    // belongs to the outer query rather than to this function's namespace.
    crate::plan::rewrite_scalar_expression(expression, &mut |expression| {
        if let ScalarExpr::Param(index) = expression {
            *expression = arguments[*index - 1].clone();
        }
    });
}
