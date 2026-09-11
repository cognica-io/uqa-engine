//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! EXECUTE argument scheduling over borrowed statement scope and assignment services.

use crate::scalar::plan::eval_physical;
use uqa_core::Value;
use uqa_sql::prepared::arguments::{
    contains_domain, immutable_argument, parameter_base_type, validate_argument,
    validate_assignment_type,
};
use uqa_sql::{plan::ExpressionPlan, ColumnType, SQLError, SQLParam, ScalarExpr};
mod context;
pub use context::{ArgumentBindingContext, PreparedArgumentScopes, ScopedArgumentOperation};

pub fn bind_execute_parameters(
    scopes: &dyn PreparedArgumentScopes,
    name: &str,
    types: Option<Vec<Option<ColumnType>>>,
    arguments: &[ExpressionPlan],
    outer_parameters: &[SQLParam],
) -> Result<Vec<SQLParam>, SQLError> {
    let types = uqa_sql::prepared::execute_parameter_types(name, types, arguments.len())?;
    if types.is_empty() {
        return Ok(Vec::new());
    }
    scopes.with_scope(outer_parameters, &mut |context| {
        bind_arguments(context, arguments, &types)
    })
}

struct AnalyzedArgument {
    source: Option<ColumnType>,
    constant: Option<Value>,
    converted: bool,
}

fn bind_arguments(
    context: ArgumentBindingContext<'_>,
    arguments: &[ExpressionPlan],
    types: &[Option<ColumnType>],
) -> Result<Vec<SQLParam>, SQLError> {
    // Resolve names, types, and constant input conversions before evaluating
    // expressions that can consume sequence values or otherwise change state.
    let mut analyzed = arguments
        .iter()
        .zip(types)
        .enumerate()
        .map(|(index, (argument, target))| {
            validate_argument(context.validation.aggregates, argument)?;
            let source = if matches!(
                argument.scalar,
                ScalarExpr::Literal(Value::Str(_) | Value::Null)
            ) {
                None
            } else {
                (context.analyze_type)(argument)?
            };
            if let (Some(source), Some(target)) = (&source, target) {
                validate_assignment_type(index, source, target)?;
            }
            let constant = match (&argument.scalar, target) {
                (ScalarExpr::Literal(Value::Str(value)), Some(target)) => {
                    Some(uqa_sql::assignment::conversion::coerce_assignment_value(
                        context.assignment,
                        Value::Str(value.clone()),
                        parameter_base_type(target),
                        None,
                    )?)
                }
                _ => None,
            };
            let converted = constant.is_some()
                && target
                    .as_ref()
                    .is_some_and(|target| !matches!(target, ColumnType::Domain { .. }));
            Ok(AnalyzedArgument {
                source,
                constant,
                converted,
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    for ((argument, target), analyzed) in arguments.iter().zip(types).zip(&mut analyzed) {
        if analyzed.constant.is_none() && immutable_argument(&context.validation, &argument.scalar)
        {
            let value = eval_physical(argument, &context.evaluation)?;
            analyzed.constant = Some(match target {
                Some(target) if !contains_domain(parameter_base_type(target)) => {
                    uqa_sql::assignment::conversion::coerce_assignment_value(
                        context.assignment,
                        value,
                        parameter_base_type(target),
                        analyzed.source.as_ref(),
                    )?
                }
                _ => value,
            });
            analyzed.converted = target
                .as_ref()
                .is_some_and(|target| !contains_domain(target));
        }
    }
    arguments
        .iter()
        .zip(types.iter().cloned())
        .zip(analyzed)
        .map(|((argument, target), analyzed)| {
            let value = match analyzed.constant {
                Some(value) => value,
                None => eval_physical(argument, &context.evaluation)?,
            };
            match target {
                Some(target) if analyzed.converted => Ok(SQLParam::typed_scalar(value, target)),
                Some(target) => uqa_sql::assignment::conversion::coerce_assignment_value(
                    context.assignment,
                    value,
                    &target,
                    analyzed.source.as_ref(),
                )
                .map(|value| SQLParam::typed_scalar(value, target)),
                None => Ok(SQLParam::Scalar(value)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
