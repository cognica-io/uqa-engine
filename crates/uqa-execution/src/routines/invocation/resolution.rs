//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve runtime carriers through SQL overload analysis, then materialize the winning arguments.
use super::context::RoutineInvocationContext;
use crate::routines::arguments::materialize_arguments;
use std::sync::Arc;
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
/// A resolved overload plus its bound argument values.
pub(super) struct ResolvedRoutine {
    pub(super) function: Arc<SQLUserFunction>,
    pub(super) bound: Vec<Value>,
    pub(super) invocation: Box<RoutineInvocationBinding>,
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
) -> Result<Option<ResolvedRoutine>, SQLError> {
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
        invocation: matched.invocation,
    }))
}

pub(super) fn resolve_bound_routine(
    context: &RoutineInvocationContext<'_>,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
) -> Result<Option<ResolvedRoutine>, SQLError> {
    let argument_names = args
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let argument_types = if let Some(invocation) = binding
        .invocation
        .as_ref()
        .filter(|invocation| invocation.argument_sources.len() == args.len())
    {
        invocation
            .argument_sources
            .iter()
            .map(|name| {
                name.as_deref()
                    .map(|name| context.runtime.expressions.column_type_name(name))
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        runtime_argument_types(args)?
    };
    let explicit_variadic = binding.invocation.as_ref().is_some_and(|invocation| {
        matches!(
            invocation.variadic_mode,
            RoutineVariadicMode::Explicit { .. }
        )
    });
    let matched = context
        .overloads
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
    let bound = materialize_arguments(
        context.runtime.expressions,
        &matched.function.def,
        &matched.invocation,
        args,
    )?;
    Ok(Some(ResolvedRoutine {
        function: matched.function,
        bound,
        invocation: matched.invocation,
    }))
}
