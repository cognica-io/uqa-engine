//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared SQL argument analysis and assignment coercion.

use uqa_core::Value;
use uqa_execution::ScalarExpr;
use uqa_planner::ExpressionPlan;
use uqa_sql::ast::{ColumnType, FunctionVolatility};
use uqa_sql::{SQLError, SQLParam};

use super::scalar::{eval_physical, PhysicalEvalContext};
use super::{select, Engine};

pub(super) fn statement_error(sqlstate: &str, name: &str, reason: &str) -> SQLError {
    error(sqlstate, format!("prepared statement \"{name}\" {reason}"))
}

struct AnalyzedArgument {
    source: Option<ColumnType>,
    constant: Option<Value>,
    converted: bool,
}

pub(super) fn bind_execute_parameters(
    engine: &Engine,
    name: &str,
    arguments: &[ExpressionPlan],
    outer_parameters: &[SQLParam],
) -> Result<Vec<SQLParam>, SQLError> {
    let types = engine
        .prepared_parameter_types(name)
        .ok_or_else(|| statement_error("26000", name, "does not exist"))?;
    if arguments.len() != types.len() {
        return Err(error(
            "42601",
            format!("wrong number of parameters for prepared statement \"{name}\""),
        ));
    }
    let scope = select::CteScope::new_for_current_routine(engine);
    let hook = select::ScopedEngineHook::new(engine, &scope);
    let context = PhysicalEvalContext::new(None, outer_parameters)
        .with_function_hook(&hook)
        .with_subquery_runner(&hook);
    // Resolve names, types, and constant input conversions before evaluating
    // expressions that can consume sequence values or otherwise change state.
    let mut analyzed = arguments
        .iter()
        .zip(&types)
        .enumerate()
        .map(|(index, (argument, target))| {
            validate_argument(engine, argument)?;
            let source = if matches!(
                argument.scalar,
                ScalarExpr::Literal(Value::Str(_) | Value::Null)
            ) {
                None
            } else {
                select::analyze_expression_plan_type(engine, argument, outer_parameters, &scope)?
            };
            if let (Some(source), Some(target)) = (&source, target) {
                validate_assignment_type(index, source, target)?;
            }
            let constant = match (&argument.scalar, target) {
                (ScalarExpr::Literal(Value::Str(value)), Some(target)) => {
                    Some(super::ddl::coerce_assignment_value(
                        engine,
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
    for ((argument, target), analyzed) in arguments.iter().zip(&types).zip(&mut analyzed) {
        if analyzed.constant.is_none() && immutable_argument(engine, &argument.scalar) {
            let value = eval_physical(argument, &context)?;
            analyzed.constant = Some(match target {
                Some(target) if !contains_domain(parameter_base_type(target)) => {
                    super::ddl::coerce_assignment_value(
                        engine,
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
        .zip(types)
        .zip(analyzed)
        .map(|((argument, target), analyzed)| {
            let value = match analyzed.constant {
                Some(value) => value,
                None => eval_physical(argument, &context)?,
            };
            match target {
                Some(target) if analyzed.converted => Ok(SQLParam::typed_scalar(value, target)),
                Some(target) => super::ddl::coerce_assignment_value(
                    engine,
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

fn validate_assignment_type(
    index: usize,
    source: &ColumnType,
    target: &ColumnType,
) -> Result<(), SQLError> {
    if !uqa_execution::assignment_type_compatible(source, target) {
        return Err(error(
            "42804",
            format!(
                "parameter ${} of type {} cannot be coerced to the expected type {}",
                index + 1,
                source.sql_name(),
                target.sql_name()
            ),
        ));
    }
    Ok(())
}

fn parameter_base_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    ty
}

fn contains_domain(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Domain { .. } => true,
        ColumnType::Array(element) => contains_domain(element),
        _ => false,
    }
}

fn immutable_argument(engine: &Engine, expression: &ScalarExpr) -> bool {
    let mut immutable = true;
    expression.visit(&mut |expression| match expression {
        ScalarExpr::Param(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::Position(_) => immutable = false,
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            immutable &= super::volatility::function_volatility_with_binding(
                engine,
                name,
                binding.as_ref(),
                args.len(),
            ) == FunctionVolatility::Immutable;
        }
        ScalarExpr::Cast { ty, .. } => {
            immutable &= !super::resolve_catalog_column_type(engine, ty)
                .as_ref()
                .is_some_and(contains_domain);
        }
        _ => {}
    });
    immutable
}

fn validate_argument(engine: &Engine, argument: &ExpressionPlan) -> Result<(), SQLError> {
    if !argument.subqueries.is_empty() {
        return Err(error("0A000", "cannot use subquery in EXECUTE parameter"));
    }
    if argument.scalar.contains_window() {
        return Err(error(
            "42P20",
            "window functions are not allowed in EXECUTE parameters",
        ));
    }
    if super::aggregates::contains_aggregate(engine, &argument.scalar) {
        return Err(error(
            "42803",
            "aggregate functions are not allowed in EXECUTE parameters",
        ));
    }
    Ok(())
}

fn error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
