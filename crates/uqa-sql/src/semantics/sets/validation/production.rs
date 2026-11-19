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
    crate::catalog::work::inline(expression_may_return_set_with_work(
        engine,
        resolver,
        expression,
        schema,
        params,
        control,
        &mut (),
    ))
}

/// Window arguments have their own placement rules. Ordinary scalar arguments, call ordering and filters retain their original first-match precedence.
pub(crate) async fn expression_may_return_set_with_work(
    engine: &dyn RoutineResolution,
    resolver: &dyn FunctionTypeResolver,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::work::Work + Send),
) -> Result<bool, SQLError> {
    use crate::ir::observation::{visit, Decision};
    visit(expression, control, work, true, &mut |node| {
        control.check()?;
        if let ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } = node
        {
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
            )? {
                return Ok(Decision::Stop);
            }
        }
        Ok(Decision::Descend)
    })
    .await
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
