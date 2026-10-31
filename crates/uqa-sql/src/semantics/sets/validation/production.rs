//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Full set-returning overload analysis retains the same observation order under the caller's planning owner.

use crate::{
    ast::FunctionBinding, routines::RoutineResolution, FunctionTypeResolver, RowSchema, SQLError,
    SQLParam, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub struct SetFunctionAnalysis<'a> {
    pub routines: &'a dyn RoutineResolution,
    pub types: &'a dyn FunctionTypeResolver,
}

pub fn function_may_return_set_with_control(
    context: SetFunctionAnalysis<'_>,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let engine = context.routines;
    let resolver = context.types;
    control.check()?;
    if binding.and_then(|binding| binding.dispatch).is_some()
        || binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax)
    {
        return Ok(false);
    }
    if binding.is_some_and(|binding| !binding.builtin) {
        let signature = crate::function_call_argument_signature_with_control(
            args,
            schema,
            params,
            Some(resolver),
            control,
        )?;
        return selected_setness(engine, name, binding, &signature, control)
            .map(|setness| setness.unwrap_or(false));
    }
    let identity = crate::semantics::lowercase_function_name_with_control(name, control)?;
    let builtin =
        crate::semantics::builtin_function_dispatch_name_with_control(identity.as_ref(), control)?;
    let builtin_returns_set = super::builtin_returns_set(builtin.as_ref())
        || engine.has_registered_table_function_with_control(identity.as_ref(), control)?;
    if builtin_returns_set
        && (binding.is_some()
            || !engine
                .visible_function_setness_with_control(name, control)?
                .found)
    {
        return Ok(true);
    }
    let signature = crate::function_call_argument_signature_with_control(
        args,
        schema,
        params,
        Some(resolver),
        control,
    )?;
    let visible = engine.visible_function_setness_with_control(name, control)?;
    if !visible.found {
        return Ok(false);
    }
    if !builtin_returns_set && !crate::is_fixed_builtin(name) {
        if let Some(setness) = visible.uniform {
            return Ok(setness);
        }
    }
    if let Some(resolved) = crate::type_resolution::resolve_fixed_builtin_call_with_control(
        name,
        binding,
        &signature.0,
        &signature.1,
        signature.2,
        Some(resolver),
        control,
    )? {
        if resolved.selected.binding.builtin {
            return Ok(builtin_returns_set);
        }
        return selected_setness(
            engine,
            name,
            Some(&resolved.selected.binding),
            &signature,
            control,
        )
        .map(|setness| setness.unwrap_or(false));
    }
    match selected_setness(engine, name, binding, &signature, control) {
        Ok(Some(setness)) => Ok(setness),
        Ok(None) => Ok(builtin_returns_set),
        Err(error) if builtin_returns_set && error.sqlstate() == Some("42883") => Ok(true),
        Err(error) if binding.is_none() && error.sqlstate() == Some("42883") => {
            match crate::type_resolution::builtin_function_type_with_resolver_and_control(
                builtin.as_ref(),
                args,
                &[],
                schema,
                params,
                resolver,
                control,
            ) {
                Ok(Some(_)) => Ok(false),
                Err(resource) if matches!(resource.sqlstate(), Some("53200" | "57014")) => {
                    Err(resource)
                }
                Ok(None) | Err(_) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn selected_setness(
    engine: &dyn RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
    signature: &(Vec<Option<String>>, Vec<Option<crate::ColumnType>>, bool),
    control: &ProductionControl<'_>,
) -> Result<Option<bool>, SQLError> {
    engine.resolve_static_sql_function_setness_with_control(
        name,
        binding,
        &signature.0,
        &signature.1,
        signature.2,
        control,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub fn expression_may_return_set_with_control(
    engine: &dyn crate::routines::RoutineResolution,
    resolver: &dyn FunctionTypeResolver,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    match expression {
        ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            filter,
            ..
        } => {
            if function_may_return_set_with_control(
                SetFunctionAnalysis {
                    routines: engine,
                    types: resolver,
                },
                name,
                binding.as_ref(),
                args,
                schema,
                params,
                control,
            )? || expressions_may_return_set_with_control(
                engine, resolver, args, schema, params, control,
            )? || expressions_may_return_set_with_control(
                engine,
                resolver,
                order_by.iter().map(|order| &order.expr),
                schema,
                params,
                control,
            )? {
                return Ok(true);
            }
            filter.as_deref().map_or(Ok(false), |filter| {
                expression_may_return_set_with_control(
                    engine, resolver, filter, schema, params, control,
                )
            })
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => expressions_may_return_set_with_control(
            engine, resolver, items, schema, params, control,
        ),
        ScalarExpr::Binary { lhs, rhs, .. } => Ok(expression_may_return_set_with_control(
            engine, resolver, lhs, schema, params, control,
        )? || expression_may_return_set_with_control(
            engine, resolver, rhs, schema, params, control,
        )?),
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => {
            expression_may_return_set_with_control(engine, resolver, inner, schema, params, control)
        }
        ScalarExpr::Between { expr, low, high } => {
            Ok(expression_may_return_set_with_control(
                engine, resolver, expr, schema, params, control,
            )? || expression_may_return_set_with_control(
                engine, resolver, low, schema, params, control,
            )? || expression_may_return_set_with_control(
                engine, resolver, high, schema, params, control,
            )?)
        }
        ScalarExpr::InList { expr, list, .. } => Ok(expression_may_return_set_with_control(
            engine, resolver, expr, schema, params, control,
        )? || expressions_may_return_set_with_control(
            engine, resolver, list, schema, params, control,
        )?),
        // A window call returns one value per row: its arguments, FILTER and frame admit no set-returning call, and its partition and ordering keys expand the rows below the window, as `expression_returns_set` treats `WindowFunc`.
        ScalarExpr::WindowCall { .. } => Ok(false),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                if expression_may_return_set_with_control(
                    engine, resolver, base, schema, params, control,
                )? {
                    return Ok(true);
                }
            }
            for (condition, result) in when {
                if expression_may_return_set_with_control(
                    engine, resolver, condition, schema, params, control,
                )? || expression_may_return_set_with_control(
                    engine, resolver, result, schema, params, control,
                )? {
                    return Ok(true);
                }
            }
            if let Some(branch) = else_branch {
                if expression_may_return_set_with_control(
                    engine, resolver, branch, schema, params, control,
                )? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        ScalarExpr::InSubquery { expr, .. } => {
            expression_may_return_set_with_control(engine, resolver, expr, schema, params, control)
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => Ok(false),
    }
}

pub fn expressions_may_return_set_with_control<'a>(
    engine: &dyn crate::routines::RoutineResolution,
    resolver: &dyn FunctionTypeResolver,
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    for expression in expressions {
        if expression_may_return_set_with_control(
            engine, resolver, expression, schema, params, control,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}
