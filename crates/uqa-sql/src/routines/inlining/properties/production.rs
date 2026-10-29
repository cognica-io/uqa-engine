//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volatility and duplication eligibility retain only admitted observation scratch.

use super::{
    rank, FunctionVolatility, Properties, RoutineInliningContext, RowSchema, SQLError, SQLParam,
    ScalarExpr,
};
use crate::{ast::FunctionBinding, ColumnType};
use uqa_core::memory::ProductionControl;

pub(in crate::routines::inlining) fn can_duplicate_with_control(
    context: &RoutineInliningContext<'_>,
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let properties = inspect_with_control(
        context,
        expression,
        &[],
        &RowSchema::empty_with_control(control)?,
        control,
    )?;
    Ok(!properties.subquery
        && properties.cost <= 10.0
        && properties.volatility != FunctionVolatility::Volatile)
}

pub(in crate::routines::inlining) fn inspect_with_control(
    context: &RoutineInliningContext<'_>,
    expression: &ScalarExpr,
    parameters: &[SQLParam],
    schema: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Properties, SQLError> {
    control.check()?;
    let mut properties = Properties {
        volatility: FunctionVolatility::Immutable,
        nonstrict: false,
        subquery: false,
        set_returning: false,
        cost: 0.0,
    };
    let mut failure = None;
    expression.visit(&mut |node| {
        if failure.is_some() {
            return;
        }
        if let Err(error) =
            inspect_node(context, node, parameters, schema, &mut properties, control)
        {
            failure = Some(error);
        }
    });
    failure.map_or(Ok(properties), Err)
}

fn inspect_node(
    context: &RoutineInliningContext<'_>,
    expression: &ScalarExpr,
    parameters: &[SQLParam],
    schema: &RowSchema,
    properties: &mut Properties,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    use super::super::operators;
    control.check()?;
    let mut volatility = FunctionVolatility::Immutable;
    match expression {
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            volatility = inspect_call(
                context,
                name,
                binding.as_ref(),
                args,
                parameters,
                schema,
                properties,
                control,
            )?;
        }
        ScalarExpr::Cast { expr, ty, .. } => {
            let target = context
                .types
                .resolve_catalog_column_type_name_with_control(ty, control)?;
            let source = operators::optional_type(context, expr, parameters, schema, control)?;
            volatility = match source.as_ref() {
                Some(source) => {
                    crate::type_resolution::cast_volatility_with_control(source, &target, control)?
                }
                None => FunctionVolatility::Stable,
            };
            properties.nonstrict |= matches!(*target, ColumnType::Domain { .. });
            if source.as_deref() != Some(&*target) {
                properties.cost += 1.0;
            }
        }
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => {
            properties.subquery = true;
            properties.nonstrict = true;
        }
        ScalarExpr::WindowCall { .. } => {
            properties.set_returning = true;
            properties.nonstrict = true;
        }
        ScalarExpr::Array(_)
        | ScalarExpr::Row(_)
        | ScalarExpr::CompositeRow { .. }
        | ScalarExpr::And(_)
        | ScalarExpr::Or(_)
        | ScalarExpr::IsNull { .. }
        | ScalarExpr::Case { .. } => properties.nonstrict = true,
        ScalarExpr::Binary { op, lhs, rhs } => {
            volatility = operators::volatility_with_control(
                context, *op, lhs, rhs, parameters, schema, control,
            )?;
            properties.cost += 1.0;
        }
        ScalarExpr::Between { expr, low, high } => {
            properties.nonstrict = true;
            volatility = operators::comparisons_with_control(
                context,
                crate::ast::BinaryOp::LessEqual,
                [
                    (low.as_ref(), expr.as_ref()),
                    (expr.as_ref(), high.as_ref()),
                ],
                parameters,
                schema,
                control,
            )?;
            properties.cost += 2.0;
        }
        ScalarExpr::InList { expr, list, .. } => {
            properties.nonstrict |= list.len() > 1;
            volatility = operators::comparisons_with_control(
                context,
                crate::ast::BinaryOp::Equal,
                list.iter().map(|item| (expr.as_ref(), item)),
                parameters,
                schema,
                control,
            )?;
            properties.cost += if list.len() > 1 {
                list.len() as f64 * 0.5
            } else {
                1.0
            };
        }
        ScalarExpr::UnaryMinus(_) => properties.cost += 1.0,
        _ => {}
    }
    if rank(volatility) > rank(properties.volatility) {
        properties.volatility = volatility;
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "routine properties require both the call scope and the independently borrowed production control"
)]
fn inspect_call(
    context: &RoutineInliningContext<'_>,
    name: &str,
    binding: Option<&FunctionBinding>,
    arguments: &[ScalarExpr],
    parameters: &[SQLParam],
    schema: &RowSchema,
    properties: &mut Properties,
    control: &ProductionControl<'_>,
) -> Result<FunctionVolatility, SQLError> {
    let arity = arguments.len();
    let function = binding
        .filter(|binding| !binding.builtin)
        .map(|binding| {
            crate::routines::selected_bound_function_with_control(
                context.routines,
                binding,
                control,
            )
        })
        .transpose()?
        .flatten();
    if let Some(function) = function {
        properties.nonstrict |= !function.def.strict;
        properties.set_returning |= function.def.returns_set();
        properties.cost += f64::from(function.def.cost.unwrap_or(100.0));
        return Ok(function.def.volatility);
    }
    let volatility = match super::super::operators::concatenation_volatility_with_control(
        context, binding, arguments, parameters, schema, control,
    )? {
        Some(volatility) => volatility,
        None => crate::semantics::volatility::function_volatility_with_binding_and_control(
            context.volatility,
            name,
            binding,
            arity,
            control,
        )?,
    };
    properties.nonstrict |= strictness(name, binding, arity, control)? != Some(true);
    let dispatch = crate::semantics::builtin_function_dispatch_name_with_control(name, control)?;
    properties.set_returning |= crate::semantics::sets::validation::builtin_returns_set(&dispatch);
    properties.cost += if context
        .routines
        .has_untyped_function_with_control(name, control)?
        && binding.is_none_or(|binding| !binding.builtin)
    {
        100.0
    } else {
        1.0
    };
    Ok(volatility)
}

fn strictness(
    name: &str,
    binding: Option<&FunctionBinding>,
    arity: usize,
    control: &ProductionControl<'_>,
) -> Result<Option<bool>, SQLError> {
    if let Some(binding) = binding {
        if binding.dispatch.is_some() {
            return Ok(crate::expr::bound_scalar_function_strictness(
                name,
                Some(binding),
                arity,
            ));
        }
        if !binding.builtin {
            return Ok(None);
        }
    }
    let name = crate::semantics::builtin_function_dispatch_name_with_control(
        binding.map_or(name, |binding| &binding.name),
        control,
    )?;
    Ok(crate::expr::builtin_scalar_function_strictness(
        &name, arity,
    ))
}
