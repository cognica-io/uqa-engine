//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog overload selection keeps candidate workspace separate from the selected call's owner.

use super::{
    check_invocation_result, diagnostics, matching, matching::RoutineMatch,
    static_function_return_type_with_control,
};
use crate::{
    ast::FunctionBinding,
    routines::resolution::{RoutineCallKind, RoutineOverloadContext},
    type_resolution::{
        rank_function_matches_with_control, ResolvedFunctionOverload, RoutineCallDescriptor,
        RoutineSignatureMatchError, RoutineSignatureProductionError,
    },
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

pub(in crate::routines::resolution) fn resolve(
    resolver: &RoutineOverloadContext<'_>,
    name: &str,
    binding: Option<&FunctionBinding>,
    call: RoutineCallDescriptor<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ResolvedFunctionOverload>>, SQLError> {
    control.check()?;
    if let Some(binding) = binding {
        if binding.builtin {
            return Ok(None);
        }
        if binding.invocation.is_some() {
            return resolver
                .resolve_bound_function_overload_with_control(
                    name,
                    binding,
                    call.argument_names,
                    call.argument_types,
                    call.explicit_variadic,
                    control,
                )
                .map(Some);
        }
        let function = resolver
            .catalog
            .select_bound_routine_with_control(binding, RoutineCallKind::Function, control)?
            .ok_or_else(|| {
                diagnostics::bound(RoutineCallKind::Function, binding, control)
                    .unwrap_or_else(std::convert::identity)
            })?;
        let snapshot = resolver.catalog.routine_type_snapshot();
        let matched = matching::match_candidate(
            &snapshot,
            function,
            call,
            RoutineCallKind::Function,
            control,
        )
        .map_err(|error| signature_error(error, control))?
        .ok_or_else(|| {
            diagnostics::bound(RoutineCallKind::Function, binding, control)
                .unwrap_or_else(std::convert::identity)
        })?;
        return resolved(resolver, name, call, matched, control).map(Some);
    }
    let Some(selected) = select_unbound(resolver, name, call, control)? else {
        return Ok(None);
    };
    resolved(resolver, name, call, selected, control).map(Some)
}

pub(super) fn select_unbound(
    resolver: &RoutineOverloadContext<'_>,
    name: &str,
    call: RoutineCallDescriptor<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<RoutineMatch>, SQLError> {
    select_unbound_kind(resolver, name, call, RoutineCallKind::Function, control)
}

pub(super) fn select_unbound_kind(
    resolver: &RoutineOverloadContext<'_>,
    name: &str,
    call: RoutineCallDescriptor<'_>,
    kind: RoutineCallKind,
    control: &ProductionControl<'_>,
) -> Result<Option<RoutineMatch>, SQLError> {
    let Some(overloads) = resolver
        .catalog
        .lookup_sql_routine_candidates_with_control(name, control)?
    else {
        return Ok(None);
    };
    let (mut candidates, match_error) = collect_kind(resolver, &overloads, call, kind, control)?;
    drop(overloads);
    if candidates.values.is_empty() {
        if let Some(error) = match_error {
            return Err(diagnostics::signature(&error, control)?);
        }
        return Err(diagnostics::resolution(
            kind,
            "42883",
            name,
            RoutineCallDescriptor {
                argument_names: &[],
                ..call
            },
            "does not exist",
            control,
        )?);
    }
    drop(match_error);
    if !rank_function_matches_with_control(&mut candidates.values, call.argument_types, control)?
        || candidates.values.len() != 1
    {
        return Err(diagnostics::resolution(
            kind,
            "42725",
            name,
            RoutineCallDescriptor {
                argument_names: &[],
                ..call
            },
            "is not unique",
            control,
        )?);
    }
    let selected = candidates.values.pop().expect("one selected routine");
    drop(candidates);
    Ok(Some(selected))
}

pub(super) fn signature_error(
    error: RoutineSignatureProductionError,
    control: &ProductionControl<'_>,
) -> SQLError {
    match error {
        RoutineSignatureProductionError::Production(error) => error,
        RoutineSignatureProductionError::Signature(error) => {
            diagnostics::signature(&error, control).unwrap_or_else(std::convert::identity)
        }
    }
}

pub(super) fn collect(
    resolver: &RoutineOverloadContext<'_>,
    overloads: &[std::sync::Arc<crate::routines::SQLUserFunction>],
    call: RoutineCallDescriptor<'_>,
    control: &ProductionControl<'_>,
) -> Result<
    (
        crate::type_resolution::RankingCandidates<RoutineMatch>,
        Option<Produced<RoutineSignatureMatchError>>,
    ),
    SQLError,
> {
    collect_kind(
        resolver,
        overloads,
        call,
        RoutineCallKind::Function,
        control,
    )
}

fn collect_kind(
    resolver: &RoutineOverloadContext<'_>,
    overloads: &[std::sync::Arc<crate::routines::SQLUserFunction>],
    call: RoutineCallDescriptor<'_>,
    kind: RoutineCallKind,
    control: &ProductionControl<'_>,
) -> Result<
    (
        crate::type_resolution::RankingCandidates<RoutineMatch>,
        Option<Produced<RoutineSignatureMatchError>>,
    ),
    SQLError,
> {
    let snapshot = resolver.catalog.routine_type_snapshot();
    let mut candidates = crate::type_resolution::Candidates::new(control);
    let mut match_error = None;
    for function in overloads {
        control.check()?;
        match matching::match_candidate(
            &snapshot,
            std::sync::Arc::clone(function),
            call,
            kind,
            control,
        ) {
            Ok(Some(matched)) => candidates.push(matched)?,
            Ok(None) => {}
            Err(RoutineSignatureProductionError::Signature(error)) => {
                match_error.get_or_insert(error);
            }
            Err(RoutineSignatureProductionError::Production(error)) => return Err(error),
        }
    }
    let mut candidates = candidates.finish();
    matching::retain_earliest_effective_signatures(&mut candidates.values, control)?;
    Ok((candidates, match_error))
}

pub(super) fn resolved(
    resolver: &RoutineOverloadContext<'_>,
    name: &str,
    call: RoutineCallDescriptor<'_>,
    matched: RoutineMatch,
    control: &ProductionControl<'_>,
) -> Result<Produced<ResolvedFunctionOverload>, SQLError> {
    diagnostics::kind(
        name,
        call,
        RoutineCallKind::Function,
        &matched.function.def,
        control,
    )?;
    if let Some(error) = &matched.signature.default_error {
        return Err(diagnostics::default_type(resolver.catalog, error, control)?);
    }
    check_invocation_result(resolver, &matched.invocation, control)?;
    let result = static_function_return_type_with_control(
        resolver,
        name,
        &matched.function.def,
        Some(&matched.invocation),
        control,
    )?;
    let precedes_pg_catalog = resolver
        .user_function_precedes_pg_catalog_with_control(&matched.function.def.name, control)?;
    let binding = super::selected_binding(&matched.function, matched.invocation, control)?;
    let (binding, binding_memory) = binding.into_parts();
    let (return_type, return_memory) = result.into_parts();
    Ok(control.finish(
        ResolvedFunctionOverload {
            binding,
            return_type,
            exact_matches: matched.signature.exact_matches,
            known_arguments: call.argument_types.iter().flatten().count(),
            preferred_matches: matched.signature.preferred_matches,
            precedes_pg_catalog,
        },
        control.combine(binding_memory, return_memory),
    )?)
}
