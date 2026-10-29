//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` candidate selection across catalog routines and built-ins.

#[cfg(test)]
mod tests;

use super::RoutineOverloadContext;
use crate::ast::{ColumnType, FunctionBinding};
use crate::type_resolution::{BuiltinFunctionOverload, ResolvedFunctionOverload};
use crate::SQLError;

#[derive(Clone, Copy)]
enum ResolutionContext {
    Scalar,
    Table,
}

struct ResolutionRequest<'a> {
    resolver: &'a RoutineOverloadContext<'a>,
    name: &'a str,
    argument_names: &'a [Option<String>],
    argument_types: &'a [Option<ColumnType>],
    explicit_variadic: bool,
    builtins: &'a [BuiltinFunctionOverload],
    context: ResolutionContext,
}

pub(super) fn resolve(
    resolver: &RoutineOverloadContext<'_>,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    builtins: &[BuiltinFunctionOverload],
) -> Result<ResolvedFunctionOverload, SQLError> {
    resolve_in_context(
        &ResolutionRequest {
            resolver,
            name,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
            context: ResolutionContext::Scalar,
        },
        binding,
    )
}

pub(super) fn resolve_table(
    resolver: &RoutineOverloadContext<'_>,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    builtins: &[BuiltinFunctionOverload],
) -> Result<ResolvedFunctionOverload, SQLError> {
    resolve_in_context(
        &ResolutionRequest {
            resolver,
            name,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
            context: ResolutionContext::Table,
        },
        binding,
    )
}

fn resolve_in_context(
    request: &ResolutionRequest<'_>,
    binding: Option<&FunctionBinding>,
) -> Result<ResolvedFunctionOverload, SQLError> {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let selected = super::production::combined::resolve(
        request.resolver,
        request.name,
        binding,
        crate::type_resolution::RoutineCallDescriptor {
            argument_names: request.argument_names,
            argument_types: request.argument_types,
            explicit_variadic: request.explicit_variadic,
        },
        request.builtins,
        &control,
    )?
    .into_uncontrolled()
    .expect("ordinary combined overload resolution");
    if matches!(request.context, ResolutionContext::Table) {
        if let Some(binding) = binding.filter(|binding| !binding.builtin) {
            return Ok(ResolvedFunctionOverload {
                binding: binding.clone(),
                exact_matches: 0,
                preferred_matches: 0,
                ..selected
            });
        }
    }
    Ok(selected)
}
