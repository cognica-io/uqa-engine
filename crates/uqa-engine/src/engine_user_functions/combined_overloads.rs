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
    ResolvedFunctionOverload,
};
use uqa_sql::ast::{ColumnType, FunctionBinding, FunctionReturns};
use uqa_sql::SQLError;

use super::{
    retain_earliest_effective_signatures, routine_signature_types, static_function_match,
    static_function_return_type, Engine, SQLUserFunction,
};

enum FunctionTarget {
    User(Arc<SQLUserFunction>),
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
    builtins: &[BuiltinFunctionOverload],
) -> Result<ResolvedFunctionOverload, SQLError> {
    resolve_in_context(
        engine,
        name,
        binding,
        argument_names,
        argument_types,
        builtins,
        ResolutionContext::Scalar,
    )
}

pub(super) fn resolve_table(
    engine: &Engine,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtins: &[BuiltinFunctionOverload],
) -> Result<ResolvedFunctionOverload, SQLError> {
    resolve_in_context(
        engine,
        name,
        binding,
        argument_names,
        argument_types,
        builtins,
        ResolutionContext::Table,
    )
}

fn resolve_in_context(
    engine: &Engine,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtins: &[BuiltinFunctionOverload],
    context: ResolutionContext,
) -> Result<ResolvedFunctionOverload, SQLError> {
    if let Some(binding) = binding {
        return resolve_bound(
            engine,
            name,
            binding,
            argument_names,
            argument_types,
            builtins,
            context,
        );
    }
    let users = engine
        .lookup_sql_routine_candidates(name)
        .unwrap_or_default();
    let builtins = builtins
        .iter()
        .filter(|builtin| builtin_name_matches(name, &builtin.name))
        .cloned()
        .collect::<Vec<_>>();
    resolve_candidates(
        engine,
        name,
        users,
        builtins,
        argument_names,
        argument_types,
        context,
    )
}

fn resolve_bound(
    engine: &Engine,
    name: &str,
    binding: &FunctionBinding,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtins: &[BuiltinFunctionOverload],
    context: ResolutionContext,
) -> Result<ResolvedFunctionOverload, SQLError> {
    if !binding.builtin {
        if matches!(context, ResolutionContext::Scalar) {
            return <Engine as FunctionTypeResolver>::resolve_function_overload(
                engine,
                name,
                Some(binding),
                argument_names,
                argument_types,
            )?
            .ok_or_else(|| bound_function_resolution_error(binding));
        }
        let function = engine
            .resolve_static_sql_function(name, Some(binding), argument_names, argument_types)?
            .ok_or_else(|| bound_function_resolution_error(binding))?;
        return Ok(ResolvedFunctionOverload {
            binding: binding.clone(),
            return_type: table_function_return_type(name, &function.def)?,
            exact_matches: 0,
            known_arguments: argument_types.iter().flatten().count(),
            preferred_matches: 0,
            precedes_pg_catalog: engine.user_function_precedes_pg_catalog(&function.def.name),
        });
    }
    let builtin = builtins
        .iter()
        .find(|builtin| builtin_binding_matches(builtin, binding))
        .ok_or_else(|| bound_function_resolution_error(binding))?;
    let matched = builtin_function_match(builtin.clone(), argument_names, argument_types)
        .ok_or_else(|| bound_function_resolution_error(binding))?;
    let mut resolved = resolved_builtin_overload(builtin.clone(), argument_types);
    resolved.exact_matches = matched.exact_matches;
    resolved.preferred_matches = matched.preferred_matches;
    Ok(resolved)
}

fn resolve_candidates(
    engine: &Engine,
    name: &str,
    users: Vec<Arc<SQLUserFunction>>,
    builtins: Vec<BuiltinFunctionOverload>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    context: ResolutionContext,
) -> Result<ResolvedFunctionOverload, SQLError> {
    let procedure_matches = users.iter().any(|function| {
        function.def.is_procedure
            && static_function_match(function.clone(), argument_names, argument_types).is_some()
    });
    let mut matched_users = users
        .into_iter()
        .filter(|function| !function.def.is_procedure)
        .filter_map(|function| static_function_match(function, argument_names, argument_types))
        .collect::<Vec<_>>();
    retain_earliest_effective_signatures(&mut matched_users);
    let mut user_candidates = matched_users
        .into_iter()
        .map(|matched| FunctionMatch {
            target: FunctionTarget::User(matched.function),
            argument_types: matched.argument_types,
            raw_exact_matches: matched.raw_exact_matches,
            exact_matches: matched.exact_matches,
            preferred_matches: matched.preferred_matches,
        })
        .collect::<Vec<_>>();
    let mut builtin_candidates = builtins
        .into_iter()
        .filter_map(|builtin| builtin_function_match(builtin, argument_names, argument_types))
        .collect::<Vec<_>>();

    let builtin_signatures = builtin_candidates
        .iter()
        .map(|candidate| candidate.argument_types.clone())
        .collect::<Vec<_>>();
    let user_signatures_preceding_pg_catalog = user_candidates
        .iter()
        .filter_map(|candidate| match &candidate.target {
            FunctionTarget::User(function)
                if engine.user_function_precedes_pg_catalog(&function.def.name) =>
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
        let FunctionTarget::User(function) = &candidate.target else {
            return true;
        };
        engine.user_function_precedes_pg_catalog(&function.def.name)
            || !builtin_signatures.contains(&candidate.argument_types)
    });

    let mut candidates = user_candidates;
    candidates.extend(builtin_candidates);
    if candidates.is_empty() {
        return Err(resolution_error(
            if procedure_matches { "42809" } else { "42883" },
            name,
            argument_names,
            argument_types,
            if procedure_matches {
                "is a procedure"
            } else {
                "does not exist"
            },
        ));
    }

    if !rank_function_matches(&mut candidates, argument_types) || candidates.len() != 1 {
        return Err(resolution_error(
            "42725",
            name,
            argument_names,
            argument_types,
            "is not unique",
        ));
    }

    let selected = candidates
        .pop()
        .ok_or_else(|| SQLError::Internal("resolved function candidate disappeared".into()))?;
    let known_arguments = argument_types.iter().flatten().count();
    match selected.target {
        FunctionTarget::User(function) => Ok(ResolvedFunctionOverload {
            binding: FunctionBinding {
                name: function.def.name.clone(),
                argument_types: routine_signature_types(&function.def),
                builtin: false,
            },
            return_type: match context {
                ResolutionContext::Scalar => static_function_return_type(name, &function.def)?,
                ResolutionContext::Table => table_function_return_type(name, &function.def)?,
            },
            exact_matches: selected.exact_matches,
            known_arguments,
            preferred_matches: selected.preferred_matches,
            precedes_pg_catalog: engine.user_function_precedes_pg_catalog(&function.def.name),
        }),
        FunctionTarget::Builtin(builtin) => {
            let mut resolved = resolved_builtin_overload(builtin, argument_types);
            resolved.exact_matches = selected.exact_matches;
            resolved.preferred_matches = selected.preferred_matches;
            Ok(resolved)
        }
    }
}

fn table_function_return_type(
    name: &str,
    definition: &uqa_sql::ast::CreateFunction,
) -> Result<ColumnType, SQLError> {
    if matches!(definition.returns, FunctionReturns::Table) {
        Ok(ColumnType::Record)
    } else {
        static_function_return_type(name, definition)
    }
}

fn builtin_function_match(
    builtin: BuiltinFunctionOverload,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> Option<FunctionMatch> {
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
