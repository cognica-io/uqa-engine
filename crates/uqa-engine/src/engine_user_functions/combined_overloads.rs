//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` candidate selection across catalog routines and built-ins.

use std::sync::Arc;

use uqa_execution::{
    builtin_binding_matches, builtin_name_matches, match_builtin_function_overload,
    rank_function_matches, BuiltinFunctionOverload, FunctionTypeResolver, RankedFunctionMatch,
    ResolvedFunctionOverload, RoutineSignatureMatchError,
};
use uqa_sql::ast::{ColumnType, FunctionBinding, FunctionReturns};
use uqa_sql::SQLError;

use super::resolution::{
    retain_earliest_effective_signatures, static_function_match, static_function_return_type,
    static_signature_error, RoutineCallKind, StaticFunctionMatch,
};
use super::SQLUserFunction;
use crate::Engine;

enum FunctionTarget {
    User(StaticFunctionMatch),
    Builtin(BuiltinFunctionOverload),
}

struct FunctionMatch {
    target: FunctionTarget,
    argument_types: Vec<String>,
    raw_exact_matches: usize,
    exact_matches: usize,
    preferred_matches: usize,
}

#[derive(Clone, Copy)]
enum ResolutionContext {
    Scalar,
    Table,
}

struct ResolutionRequest<'a> {
    engine: &'a Engine,
    name: &'a str,
    argument_names: &'a [Option<String>],
    argument_types: &'a [Option<ColumnType>],
    explicit_variadic: bool,
    builtins: &'a [BuiltinFunctionOverload],
    context: ResolutionContext,
}

struct UserCandidateSet {
    candidates: Vec<FunctionMatch>,
    procedure_matches: bool,
    match_error: Option<RoutineSignatureMatchError>,
}

impl RankedFunctionMatch for FunctionMatch {
    fn argument_types(&self) -> &[String] {
        &self.argument_types
    }

    fn raw_exact_matches(&self) -> usize {
        self.raw_exact_matches
    }

    fn exact_matches(&self) -> usize {
        self.exact_matches
    }

    fn preferred_matches(&self) -> usize {
        self.preferred_matches
    }
}

pub(super) fn resolve(
    engine: &Engine,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    builtins: &[BuiltinFunctionOverload],
) -> Result<ResolvedFunctionOverload, SQLError> {
    resolve_in_context(
        &ResolutionRequest {
            engine,
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
    engine: &Engine,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
    builtins: &[BuiltinFunctionOverload],
) -> Result<ResolvedFunctionOverload, SQLError> {
    resolve_in_context(
        &ResolutionRequest {
            engine,
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
    if let Some(binding) = binding {
        return resolve_bound(request, binding);
    }
    let users = request
        .engine
        .lookup_sql_routine_candidates(request.name)?
        .unwrap_or_default();
    let builtins = request
        .builtins
        .iter()
        .filter(|builtin| builtin_name_matches(request.name, &builtin.name))
        .cloned()
        .collect::<Vec<_>>();
    resolve_candidates(request, users, builtins)
}

fn resolve_bound(
    request: &ResolutionRequest<'_>,
    binding: &FunctionBinding,
) -> Result<ResolvedFunctionOverload, SQLError> {
    if !binding.builtin {
        if matches!(request.context, ResolutionContext::Scalar) {
            return <Engine as FunctionTypeResolver>::resolve_function_overload(
                request.engine,
                request.name,
                Some(binding),
                request.argument_names,
                request.argument_types,
                request.explicit_variadic,
            )?
            .ok_or_else(|| bound_function_resolution_error(binding));
        }
        let function = request
            .engine
            .resolve_static_sql_function(
                request.name,
                Some(binding),
                request.argument_names,
                request.argument_types,
                request.explicit_variadic,
            )?
            .ok_or_else(|| bound_function_resolution_error(binding))?;
        return Ok(ResolvedFunctionOverload {
            binding: binding.clone(),
            return_type: table_function_return_type(
                request.engine,
                request.name,
                &function.def,
                binding.invocation.as_deref(),
            )?,
            exact_matches: 0,
            known_arguments: request.argument_types.iter().flatten().count(),
            preferred_matches: 0,
            precedes_pg_catalog: request
                .engine
                .user_function_precedes_pg_catalog(&function.def.name),
        });
    }
    let builtin = request
        .builtins
        .iter()
        .find(|builtin| builtin_binding_matches(builtin, binding))
        .ok_or_else(|| bound_function_resolution_error(binding))?;
    let matched = builtin_function_match(
        builtin.clone(),
        request.argument_names,
        request.argument_types,
        request.explicit_variadic,
    )
    .ok_or_else(|| bound_function_resolution_error(binding))?;
    let mut resolved = resolved_builtin_overload(builtin.clone(), request.argument_types);
    resolved.exact_matches = matched.exact_matches;
    resolved.preferred_matches = matched.preferred_matches;
    Ok(resolved)
}

fn resolve_candidates(
    request: &ResolutionRequest<'_>,
    users: Vec<Arc<SQLUserFunction>>,
    builtins: Vec<BuiltinFunctionOverload>,
) -> Result<ResolvedFunctionOverload, SQLError> {
    let UserCandidateSet {
        mut candidates,
        procedure_matches,
        match_error,
    } = collect_user_candidates(request, users);
    let mut builtin_candidates = builtins
        .into_iter()
        .filter_map(|builtin| {
            builtin_function_match(
                builtin,
                request.argument_names,
                request.argument_types,
                request.explicit_variadic,
            )
        })
        .collect::<Vec<_>>();
    apply_catalog_shadowing(request.engine, &mut candidates, &mut builtin_candidates);
    candidates.extend(builtin_candidates);
    if candidates.is_empty() {
        if let Some(error) = match_error {
            return Err(static_signature_error(
                RoutineCallKind::Function,
                request.name,
                error,
            ));
        }
        return Err(resolution_error(
            if procedure_matches { "42809" } else { "42883" },
            request.name,
            request.argument_names,
            request.argument_types,
            if procedure_matches {
                "is a procedure"
            } else {
                "does not exist"
            },
        ));
    }
    if !rank_function_matches(&mut candidates, request.argument_types) || candidates.len() != 1 {
        return Err(resolution_error(
            "42725",
            request.name,
            request.argument_names,
            request.argument_types,
            "is not unique",
        ));
    }
    let selected = candidates
        .pop()
        .ok_or_else(|| SQLError::Internal("resolved function candidate disappeared".into()))?;
    resolve_selected_candidate(request, selected)
}

fn collect_user_candidates(
    request: &ResolutionRequest<'_>,
    users: Vec<Arc<SQLUserFunction>>,
) -> UserCandidateSet {
    let mut procedure_matches = false;
    let mut matched_users = Vec::new();
    let mut match_error = None;
    for function in users {
        match static_function_match(
            function.clone(),
            request.argument_names,
            request.argument_types,
            request.explicit_variadic,
        ) {
            Ok(Some(_)) if function.def.is_procedure => procedure_matches = true,
            Ok(Some(matched)) => matched_users.push(matched),
            Ok(None) => {}
            Err(error) => {
                match_error.get_or_insert(error);
            }
        }
    }
    retain_earliest_effective_signatures(&mut matched_users);
    let candidates = matched_users
        .into_iter()
        .map(|matched| FunctionMatch {
            argument_types: matched.argument_types.clone(),
            raw_exact_matches: matched.raw_exact_matches,
            exact_matches: matched.exact_matches,
            preferred_matches: matched.preferred_matches,
            target: FunctionTarget::User(matched),
        })
        .collect::<Vec<_>>();
    UserCandidateSet {
        candidates,
        procedure_matches,
        match_error,
    }
}

fn apply_catalog_shadowing(
    engine: &Engine,
    user_candidates: &mut Vec<FunctionMatch>,
    builtin_candidates: &mut Vec<FunctionMatch>,
) {
    let builtin_signatures = builtin_candidates
        .iter()
        .map(|candidate| candidate.argument_types.clone())
        .collect::<Vec<_>>();
    let user_signatures_preceding_pg_catalog = user_candidates
        .iter()
        .filter_map(|candidate| match &candidate.target {
            FunctionTarget::User(matched)
                if engine.user_function_precedes_pg_catalog(&matched.function.def.name) =>
            {
                Some(candidate.argument_types.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    builtin_candidates.retain(|candidate| {
        !user_signatures_preceding_pg_catalog.contains(&candidate.argument_types)
    });
    user_candidates.retain(|candidate| {
        let FunctionTarget::User(matched) = &candidate.target else {
            return true;
        };
        engine.user_function_precedes_pg_catalog(&matched.function.def.name)
            || !builtin_signatures.contains(&candidate.argument_types)
    });
}

fn resolve_selected_candidate(
    request: &ResolutionRequest<'_>,
    selected: FunctionMatch,
) -> Result<ResolvedFunctionOverload, SQLError> {
    let known_arguments = request.argument_types.iter().flatten().count();
    match selected.target {
        FunctionTarget::User(matched) => Ok(ResolvedFunctionOverload {
            binding: matched.binding(),
            return_type: match request.context {
                ResolutionContext::Scalar => static_function_return_type(
                    request.engine,
                    request.name,
                    &matched.function.def,
                    Some(&matched.invocation),
                )?,
                ResolutionContext::Table => table_function_return_type(
                    request.engine,
                    request.name,
                    &matched.function.def,
                    Some(&matched.invocation),
                )?,
            },
            exact_matches: selected.exact_matches,
            known_arguments,
            preferred_matches: selected.preferred_matches,
            precedes_pg_catalog: request
                .engine
                .user_function_precedes_pg_catalog(&matched.function.def.name),
        }),
        FunctionTarget::Builtin(builtin) => {
            let mut resolved = resolved_builtin_overload(builtin, request.argument_types);
            resolved.exact_matches = selected.exact_matches;
            resolved.preferred_matches = selected.preferred_matches;
            Ok(resolved)
        }
    }
}

fn table_function_return_type(
    engine: &Engine,
    name: &str,
    definition: &uqa_sql::ast::CreateFunction,
    invocation: Option<&uqa_sql::ast::RoutineInvocationBinding>,
) -> Result<ColumnType, SQLError> {
    if matches!(definition.returns, FunctionReturns::Table) {
        Ok(ColumnType::Record)
    } else {
        static_function_return_type(engine, name, definition, invocation)
    }
}

fn builtin_function_match(
    builtin: BuiltinFunctionOverload,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    explicit_variadic: bool,
) -> Option<FunctionMatch> {
    if explicit_variadic && argument_names.iter().any(Option::is_some) {
        return None;
    }
    let matched = match_builtin_function_overload(builtin, argument_names, argument_types)?;
    Some(FunctionMatch {
        target: FunctionTarget::Builtin(matched.overload),
        argument_types: matched.argument_types,
        raw_exact_matches: matched.raw_exact_matches,
        exact_matches: matched.exact_matches,
        preferred_matches: matched.preferred_matches,
    })
}

fn resolved_builtin_overload(
    builtin: BuiltinFunctionOverload,
    argument_types: &[Option<ColumnType>],
) -> ResolvedFunctionOverload {
    ResolvedFunctionOverload {
        binding: FunctionBinding {
            name: builtin.name,
            argument_types: builtin
                .argument_types
                .iter()
                .map(ColumnType::sql_name)
                .collect(),
            builtin: true,
            dispatch: None,
            invocation: None,
            resolution_error: None,
        },
        return_type: builtin.return_type,
        exact_matches: 0,
        known_arguments: argument_types.iter().flatten().count(),
        preferred_matches: 0,
        precedes_pg_catalog: false,
    }
}

fn bound_function_resolution_error(binding: &FunctionBinding) -> SQLError {
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "bound function {}({}) does not exist",
            binding.name,
            binding.argument_types.join(", ")
        ),
    }
}

fn resolution_error(
    sqlstate: &str,
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    suffix: &str,
) -> SQLError {
    let arguments = argument_names
        .iter()
        .zip(argument_types)
        .map(|(argument_name, argument_type)| {
            let argument_type = argument_type
                .as_ref()
                .map_or_else(|| "unknown".into(), ColumnType::regtype_name);
            argument_name
                .as_ref()
                .map_or(argument_type.clone(), |name| {
                    format!("{name} => {argument_type}")
                })
        })
        .collect::<Vec<_>>()
        .join(", ");
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!("function {name}({arguments}) {suffix}"),
    }
}
