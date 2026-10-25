//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical call scheduling shares SQL-owned argument, numeric and built-in semantics.

use super::{
    eval_call_arguments_with_control, eval_scalar_inner, evaluate_items, ordinary_output, plain,
    Produced, ProductionControl, ProductionVec, SQLError, ScalarEvalContext, ScalarExpr, Value,
};
use uqa_sql::ast::{FunctionBinding, FunctionDispatch, NumericOperator};

pub(super) fn evaluate_expression(
    expression: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let ScalarExpr::Func {
        name,
        binding,
        args,
        ..
    } = expression
    else {
        unreachable!("function-shaped expression")
    };
    let binding = binding.as_ref();
    if let Some(error) = binding.and_then(|binding| binding.resolution_error.as_ref()) {
        return Err(error.sql_error());
    }
    if let Some(clause) = uqa_sql::catalog::collations::expression::scalar_clause(expression)? {
        return eval_scalar_inner(clause.argument, context, control);
    }
    initialize_call(name, binding, context)?;
    if let Some(dispatch) = binding.and_then(|binding| binding.dispatch) {
        if let Some(result) = uqa_sql::expr::eval_distinct_boolean(dispatch, args.len(), |index| {
            let value = eval_scalar_inner(&args[index], context, control)?;
            Ok((*value).clone())
        }) {
            return plain(result?, control);
        }
    }
    evaluate_function(name, binding, args, context, control)
}

/// Check a call before any argument is evaluated, as `PostgreSQL`'s `ExecInitFunc` does: EXECUTE on the selected built-in, then whether the built-in returns a set, which the one value this expression needs cannot accept.
fn initialize_call(
    name: &str,
    binding: Option<&FunctionBinding>,
    context: &ScalarEvalContext<'_>,
) -> Result<(), SQLError> {
    if let (Some(binding), Some(hook)) = (binding, context.function_hook()) {
        hook.require_builtin_execute(binding)?;
    }
    if binding.is_some_and(|binding| binding.builtin)
        && uqa_sql::semantics::sets::static_setness::builtin_call_returns_set(name, binding)
    {
        return Err(uqa_sql::semantics::sets::placement::set_valued_call_error());
    }
    Ok(())
}

fn evaluate_function(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    if let Some((binding, FunctionDispatch::NumericOperator(operator))) =
        binding.and_then(|binding| binding.dispatch.map(|dispatch| (binding, dispatch)))
    {
        return numeric(operator, binding, args, context, control);
    }
    if let Some(predicate) = context.retrieval_predicate() {
        if binding.is_none_or(|binding| binding.builtin)
            && uqa_sql::semantics::retrieval_function(name)
            // This name also dispatches scalar JSONPath expressions, including paths supplied by parameters or expressions.
            && !name.eq_ignore_ascii_case("fts_match")
        {
            return plain(Value::Bool(predicate(name, args)?), control);
        }
    }
    if name.eq_ignore_ascii_case("coalesce") && binding.is_none_or(|binding| binding.builtin) {
        for argument in args {
            let value = eval_scalar_inner(argument, context, control)?;
            if !matches!(*value, Value::Null) {
                return Ok(value);
            }
        }
        return plain(Value::Null, control);
    }
    if binding.is_some_and(|binding| {
        binding.builtin && binding.dispatch == Some(FunctionDispatch::BetweenSymmetric)
    }) {
        return super::eval_between_symmetric(args, context, control);
    }
    if let Some(result) = array_access(binding, args, context, control) {
        return result;
    }
    let arguments = eval_call_arguments_with_control(args, context, control)?;
    let arguments = if binding.is_none_or(|binding| binding.builtin) {
        match super::super::call_arguments::expand_variadic_any_arguments(
            binding.map_or(name, |binding| binding.name.as_str()),
            args,
            arguments,
            control,
        )? {
            Some(arguments) => arguments,
            None => return plain(Value::Null, control),
        }
    } else {
        arguments
    };
    if control.budget().is_some() {
        return uqa_sql::expr::eval_builtin_function_call_with_control(
            name,
            binding,
            arguments,
            control,
            &context
                .sql_context()
                .with_enum_comparison_states(context.enum_comparison_states(args)),
        );
    }
    let arguments = arguments
        .into_uncontrolled()
        .expect("ordinary function arguments");
    let value = if let Some(binding) = binding {
        if binding.builtin {
            if let Some(result) = context
                .function_hook()
                .and_then(|hook| hook.call_bound_builtin_function(binding, &arguments))
            {
                return ordinary_output(result, control);
            }
            uqa_sql::expr::eval_bound_builtin_function_call(
                binding,
                arguments,
                &context
                    .sql_context()
                    .with_enum_comparison_states(context.enum_comparison_states(args)),
            )
        } else {
            bound_user_function(binding, &arguments, context, control)
        }
    } else {
        uqa_sql::expr::eval_function_call(
            name,
            arguments,
            &context
                .sql_context()
                .with_enum_comparison_states(context.enum_comparison_states(args)),
        )
    };
    ordinary_output(value, control)
}

fn bound_user_function(
    binding: &FunctionBinding,
    arguments: &[(Option<String>, Value)],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Value, SQLError> {
    let engine = context.function_hook().ok_or_else(|| {
        SQLError::Unsupported("bound user function requires a logical engine session".into())
    })?;
    context
        .routine_descriptions(binding)
        .map_or_else(
            || engine.call_bound_user_function(binding, arguments),
            |descriptions| {
                engine.call_bound_user_function_with_descriptions(
                    binding,
                    arguments,
                    descriptions,
                    control,
                )
            },
        )
        .unwrap_or_else(|| Err(SQLError::UnknownFunction(binding.name.clone())))
}

fn array_access(
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>, SQLError>> {
    let dispatch = binding.and_then(|binding| binding.dispatch)?;
    let access = uqa_sql::ast::ArrayAccess::from_dispatch(dispatch, args.len(), |index| {
        matches!(args[index], ScalarExpr::Literal(Value::Null))
    })?;
    Some(access.and_then(|access| {
        let catalog = context
            .function_hook()
            .map(uqa_sql::expr::EngineValueCatalog);
        uqa_sql::expr::eval_array_access_with_catalog_and_control(
            access,
            catalog
                .as_ref()
                .map(|catalog| catalog as &dyn uqa_sql::expr::SQLValueCatalog),
            control,
            |index| eval_scalar_inner(&args[index], context, control),
        )
    }))
}

fn numeric(
    operator: NumericOperator,
    binding: &FunctionBinding,
    args: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let values = evaluate_items(args, context, control)?;
    let slot = context.numeric_operator(args);
    if let Some(super::super::state::NumericCall::Resolved { operation, schema }) =
        slot.and_then(std::sync::OnceLock::get)
    {
        let same_schema = match (schema, context.row_schema()) {
            (None, None) => true,
            (Some(retained), Some(current)) => current.shares_index(retained),
            _ => false,
        };
        if same_schema {
            return operation.evaluate(&values, control);
        }
    }
    let mut types = ProductionVec::new(*control);
    let empty_schema = crate::RowSchema::default();
    if binding.argument_types.is_empty() {
        types.reserve(args.len())?;
        for argument in args {
            let ty = uqa_sql::common_context_type_with_control(
                argument,
                context.row_schema().unwrap_or(&empty_schema),
                context.params(),
                control,
            )?;
            let (ty, memory) = ty.map_or_else(
                || (None, control.empty_reservation()),
                |ty| {
                    let (ty, memory) = ty.into_parts();
                    (Some(ty), memory)
                },
            );
            types.push_produced(control.finish(ty, memory)?)?;
        }
    } else {
        types.reserve(binding.argument_types.len())?;
        for name in &binding.argument_types {
            let (ty, memory) =
                uqa_sql::ColumnType::from_sql_name_with_control(name, control)?.into_parts();
            types.push_produced(control.finish(Some(ty), memory)?)?;
        }
    }
    let types = types.finish()?;
    let (value, resolved) =
        uqa_sql::expr::eval_numeric_operator_with_resolution(operator, &values, &types, control)?;
    if let Some(slot) = slot.filter(|slot| slot.get().is_none()) {
        let state = resolved.map_or(
            super::super::state::NumericCall::PerEvaluation,
            |operation| super::super::state::NumericCall::Resolved {
                operation,
                schema: context.row_schema().cloned(),
            },
        );
        let _ = slot.set(state);
    }
    Ok(value)
}
