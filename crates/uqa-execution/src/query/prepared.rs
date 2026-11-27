//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! EXECUTE argument scheduling over borrowed statement scope and assignment services.

use crate::scalar::plan::eval_physical;
use uqa_core::Value;
use uqa_sql::prepared::arguments::{
    contains_domain, immutable_argument, parameter_base_type, reject_set_argument,
    validate_argument, validate_assignment_type,
};
use uqa_sql::{plan::ExpressionPlan, ColumnType, SQLError, SQLParam, ScalarExpr};
mod context;
pub use context::{ArgumentBindingContext, PreparedArgumentScopes, ScopedArgumentOperation};

/// SQL EXECUTE analyzes its written arguments in an independent parameter namespace.
pub fn bind_execute_parameters(
    scopes: &dyn PreparedArgumentScopes,
    name: &str,
    types: Option<Vec<Option<ColumnType>>>,
    arguments: &[ExpressionPlan],
) -> Result<Vec<SQLParam>, SQLError> {
    bind_parameter_expressions(scopes, name, types, arguments, &[])
}

/// Coerce supplied host values against the retained definition's parameter types.
pub(crate) fn bind_host_parameters(
    scopes: &dyn PreparedArgumentScopes,
    name: &str,
    types: Option<Vec<Option<ColumnType>>>,
    parameters: &[SQLParam],
) -> Result<Vec<SQLParam>, SQLError> {
    let arguments = (1..=parameters.len())
        .map(|index| ExpressionPlan::lower(uqa_sql::ast::Expr::Param(index)))
        .collect::<Vec<_>>();
    bind_parameter_expressions(scopes, name, types, &arguments, parameters)
}

fn bind_parameter_expressions(
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
        let types =
            uqa_sql::params::bind_declared_parameter_types(&types, context.validation.routines)?;
        let sources = if outer_parameters.is_empty() {
            None
        } else {
            Some(uqa_sql::params::bind_parameter_types(
                outer_parameters,
                context.validation.routines,
            )?)
        };
        bind_arguments(context, arguments, &types, sources.as_deref())
    })
}

struct AnalyzedArgument<'a> {
    original: &'a ExpressionPlan,
    host_type: Option<&'a ColumnType>,
    bound: Option<uqa_sql::binding::BoundExpression>,
    constant: Option<Value>,
    converted: bool,
}

impl AnalyzedArgument<'_> {
    fn expression(&self) -> &ExpressionPlan {
        self.bound
            .as_ref()
            .map_or(self.original, |bound| &bound.plan)
    }

    fn source(&self) -> Option<&ColumnType> {
        self.bound
            .as_ref()
            .and_then(|bound| bound.ty.as_deref())
            .or(self.host_type)
    }
}

fn bind_arguments(
    context: ArgumentBindingContext<'_>,
    arguments: &[ExpressionPlan],
    types: &[Option<ColumnType>],
    host_types: Option<&[Option<ColumnType>]>,
) -> Result<Vec<SQLParam>, SQLError> {
    // Resolve names, types, and constant input conversions before evaluating
    // expressions that can consume sequence values or otherwise change state.
    let mut analyzed = arguments
        .iter()
        .zip(types)
        .enumerate()
        .map(|(index, (argument, target))| {
            validate_argument(context.validation.aggregates, argument)?;
            reject_set_argument(&context.validation, argument)?;
            let host_type = host_types.and_then(|types| types[index].as_ref());
            let bound = if host_types.is_some()
                || matches!(
                    argument.scalar,
                    ScalarExpr::Literal(Value::Str(_) | Value::Null)
                ) {
                None
            } else {
                Some((context.bind_expression)(argument)?)
            };
            let source = bound
                .as_ref()
                .and_then(|bound| bound.ty.as_deref())
                .or(host_type);
            if let (Some(source), Some(target)) = (source, target) {
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
                original: argument,
                host_type,
                bound,
                constant,
                converted,
            })
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    for (analyzed, target) in analyzed.iter_mut().zip(types) {
        let argument = analyzed.expression();
        if analyzed.constant.is_none() && immutable_argument(&context.validation, &argument.scalar)
        {
            let value = eval_physical(argument, &context.evaluation)?;
            analyzed.constant = Some(match target {
                Some(target) if !contains_domain(parameter_base_type(target)) => {
                    uqa_sql::assignment::conversion::coerce_assignment_value(
                        context.assignment,
                        value,
                        parameter_base_type(target),
                        analyzed.source(),
                    )?
                }
                _ => value,
            });
            analyzed.converted = target
                .as_ref()
                .is_some_and(|target| !contains_domain(target));
        }
    }
    analyzed
        .into_iter()
        .zip(types.iter().cloned())
        .map(|(mut analyzed, target)| {
            let value = match analyzed.constant.take() {
                Some(value) => value,
                None => eval_physical(analyzed.expression(), &context.evaluation)?,
            };
            match target {
                Some(target) if analyzed.converted => Ok(SQLParam::typed_scalar(value, target)),
                Some(target) => uqa_sql::assignment::conversion::coerce_assignment_value(
                    context.assignment,
                    value,
                    &target,
                    analyzed.source(),
                )
                .map(|value| SQLParam::typed_scalar(value, target)),
                None => Ok(SQLParam::Scalar(value)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
