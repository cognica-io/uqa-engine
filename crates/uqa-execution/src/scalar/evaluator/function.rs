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

pub(super) fn evaluate_function(
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    if let Some(error) = binding.and_then(|binding| binding.resolution_error.as_ref()) {
        return Err(error.sql_error());
    }
    if let Some((binding, FunctionDispatch::NumericOperator(operator))) =
        binding.and_then(|binding| binding.dispatch.map(|dispatch| (binding, dispatch)))
    {
        return numeric(operator, binding, args, context, control);
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
    let arguments = eval_call_arguments_with_control(args, context, control)?;
    if control.budget().is_some() {
        return uqa_sql::expr::eval_generated_function_call_with_control(
            name, binding, arguments, control,
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
                &context.sql_context(),
            )
        } else {
            let sql_context = context.sql_context();
            let engine = sql_context.engine.ok_or_else(|| {
                SQLError::Unsupported(
                    "bound user function requires a logical engine session".into(),
                )
            })?;
            engine
                .call_bound_user_function(binding, &arguments)
                .unwrap_or_else(|| Err(SQLError::UnknownFunction(binding.name.clone())))
        }
    } else {
        uqa_sql::expr::eval_function_call(name, arguments, &context.sql_context())
    };
    ordinary_output(value, control)
}

fn numeric(
    operator: NumericOperator,
    binding: &FunctionBinding,
    args: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let values = evaluate_items(args, context, control)?;
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
    uqa_sql::expr::eval_numeric_operator_with_control(operator, &values, &types, control)
}
