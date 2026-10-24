//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve runtime carriers through SQL overload analysis, then materialize the winning arguments.
use super::context::RoutineInvocationContext;
use crate::routines::arguments::{
    materialize_arguments, materialize_described_arguments, ArgumentDescriptions,
};
use std::{borrow::Cow, sync::Arc};
use uqa_core::Value;
use uqa_sql::{
    ast::{ColumnType, FunctionBinding, RoutineInvocationBinding, RoutineVariadicMode},
    routines::{
        invocation::{routine_resolution_error, runtime_argument_types},
        resolution::RoutineCallKind,
        SQLUserFunction,
    },
    SQLError,
};

#[cfg(test)]
mod tests;
/// A resolved overload plus its bound argument values.
pub(super) struct ResolvedRoutine<'a> {
    pub(super) function: Arc<SQLUserFunction>,
    pub(super) bound: Vec<Value>,
    pub(super) invocation: Cow<'a, RoutineInvocationBinding>,
}

/// Resolve `name(args)` to a single overload and its bound argument
/// values (declared-type casts applied, defaults evaluated).
/// `Ok(None)` = no routine with this name at all.
pub(super) fn resolve_routine(
    context: &RoutineInvocationContext<'_>,
    name: &str,
    args: &[(Option<String>, Value)],
    declared_argument_types: Option<&[Option<ColumnType>]>,
    kind: &str,
    explicit_variadic: bool,
) -> Result<Option<ResolvedRoutine<'static>>, SQLError> {
    if context.lookup.lookup_visible_sql_functions(name)?.is_none() {
        return Ok(None);
    }
    let argument_names = args
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let inferred_argument_types: Vec<Option<ColumnType>>;
    let argument_types = match declared_argument_types {
        Some(types) if types.len() == args.len() => types,
        Some(types) => {
            return Err(SQLError::Internal(format!(
                "routine argument type count {} does not match value count {}",
                types.len(),
                args.len()
            )));
        }
        None => {
            inferred_argument_types = runtime_argument_types(args)?;
            &inferred_argument_types
        }
    };
    let call_kind = if kind == "procedure" {
        RoutineCallKind::Procedure
    } else {
        RoutineCallKind::Function
    };
    let matched = context
        .overloads
        .resolve_static_sql_routine_match(
            name,
            None,
            &argument_names,
            argument_types,
            explicit_variadic,
            call_kind,
        )?
        .ok_or_else(|| routine_resolution_error(kind, name, args, "does not exist"))?;
    let bound = materialize_arguments(
        context.runtime.expressions,
        &matched.function.def,
        &matched.invocation,
        args,
    )?;
    Ok(Some(ResolvedRoutine {
        function: matched.function,
        bound,
        invocation: Cow::Owned(*matched.invocation),
    }))
}

pub(super) fn resolve_bound_routine<'a>(
    context: &RoutineInvocationContext<'_>,
    binding: &'a FunctionBinding,
    args: &[(Option<String>, Value)],
) -> Result<Option<ResolvedRoutine<'a>>, SQLError> {
    resolve_described_bound_routine(context, binding, args, None)
}

pub(super) fn resolve_described_bound_routine<'a>(
    context: &RoutineInvocationContext<'_>,
    binding: &'a FunctionBinding,
    args: &[(Option<String>, Value)],
    retained: Option<super::context::RoutineCallRetention<'_>>,
) -> Result<Option<ResolvedRoutine<'a>>, SQLError> {
    resolve_bound_inputs(
        context.runtime.expressions,
        &context.overloads,
        retained,
        binding,
        args,
    )
}

fn resolve_bound_inputs<'a>(
    expressions: &dyn crate::routines::context::RoutineExpressions,
    overloads: &uqa_sql::routines::resolution::RoutineOverloadContext<'_>,
    retained: Option<super::context::RoutineCallRetention<'_>>,
    binding: &'a FunctionBinding,
    args: &[(Option<String>, Value)],
) -> Result<Option<ResolvedRoutine<'a>>, SQLError> {
    let scope = retained.and_then(|retained| (retained.scope)());
    if let Some(retained) = retained {
        if let Some((description, function)) = scope
            .as_ref()
            .and_then(|scope| retained.descriptions.get(binding, scope))
            .and_then(|description| {
                description
                    .function()
                    .map(|function| (description, function))
            })
        {
            if description.sources().len() == args.len() {
                let invocation = binding.invocation.as_deref().expect("retained invocation");
                let bound = materialize_described_arguments(
                    expressions,
                    &function.def,
                    invocation,
                    args,
                    Some(ArgumentDescriptions {
                        description,
                        retained,
                    }),
                )?;
                return Ok(Some(ResolvedRoutine {
                    function,
                    bound,
                    invocation: Cow::Borrowed(invocation),
                }));
            }
        }
    }
    let argument_names = args
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let argument_types = bound_argument_types(expressions, binding, args)?;
    let explicit_variadic = binding.invocation.as_ref().is_some_and(|invocation| {
        matches!(
            invocation.variadic_mode,
            RoutineVariadicMode::Explicit { .. }
        )
    });
    let matched = overloads
        .resolve_static_sql_routine_match(
            &binding.name,
            Some(binding),
            &argument_names,
            &argument_types,
            explicit_variadic,
            RoutineCallKind::Function,
        )?
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42883".into(),
            message: format!(
                "bound function {}({}) does not exist",
                binding.name,
                binding.argument_types.join(", ")
            ),
        })?;
    if let (Some(retained), Some(scope)) = (retained, scope) {
        if (retained.scope)().is_some_and(|current| scope.matches(&current)) {
            retained.descriptions.remember(
                binding,
                &matched.function,
                &argument_types,
                scope,
                &retained.control,
            )?;
        }
    }
    let descriptions = retained.and_then(|retained| {
        let scope = (retained.scope)()?;
        let description = retained.descriptions.get(binding, &scope)?;
        Some(ArgumentDescriptions {
            description,
            retained,
        })
    });
    let bound = materialize_described_arguments(
        expressions,
        &matched.function.def,
        &matched.invocation,
        args,
        descriptions,
    )?;
    Ok(Some(ResolvedRoutine {
        function: matched.function,
        bound,
        invocation: Cow::Owned(*matched.invocation),
    }))
}

fn bound_argument_types(
    expressions: &dyn crate::routines::context::RoutineExpressions,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    if let Some(invocation) = binding
        .invocation
        .as_ref()
        .filter(|invocation| invocation.argument_sources.len() == args.len())
    {
        invocation
            .argument_sources
            .iter()
            .map(|name| {
                name.as_deref()
                    .map(|name| expressions.column_type_name(name))
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()
    } else {
        runtime_argument_types(args)
    }
}
