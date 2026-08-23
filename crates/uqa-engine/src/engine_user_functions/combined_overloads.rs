//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` candidate selection across catalog routines and built-ins.

use std::sync::Arc;

use uqa_execution::{BuiltinFunctionOverload, FunctionTypeResolver, ResolvedFunctionOverload};
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::SQLError;

use super::{
    canonical_column_type_name, canonical_routine_type_name, routine_signature_types,
    routine_type_accepts_implicit_cast, routine_type_category, routine_type_is_preferred,
    static_function_match, static_function_return_type, Engine, SQLUserFunction,
};

enum FunctionTarget {
    User(Arc<SQLUserFunction>),
    Builtin(BuiltinFunctionOverload),
}

struct FunctionMatch {
    target: FunctionTarget,
    argument_types: Vec<String>,
    exact_matches: usize,
    preferred_matches: usize,
}

pub(super) fn resolve(
    engine: &Engine,
    name: &str,
    binding: Option<&FunctionBinding>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtins: &[BuiltinFunctionOverload],
) -> Result<ResolvedFunctionOverload, SQLError> {
    if let Some(binding) = binding {
        return resolve_bound(
            engine,
            name,
            binding,
            argument_names,
            argument_types,
            builtins,
        );
    }
    let users = engine.lookup_sql_functions(name).unwrap_or_default();
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
    )
}

fn resolve_bound(
    engine: &Engine,
    name: &str,
    binding: &FunctionBinding,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
    builtins: &[BuiltinFunctionOverload],
) -> Result<ResolvedFunctionOverload, SQLError> {
    if !binding.builtin {
        return <Engine as FunctionTypeResolver>::resolve_function_overload(
            engine,
            name,
            Some(binding),
            argument_names,
            argument_types,
        )?
        .ok_or_else(|| bound_function_resolution_error(binding));
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
    mut builtins: Vec<BuiltinFunctionOverload>,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> Result<ResolvedFunctionOverload, SQLError> {
    let mut visible_users = Vec::with_capacity(users.len());
    for function in users {
        let signature = routine_signature_types(&function.def);
        let same_builtin = builtins.iter().any(|builtin| {
            builtin
                .argument_types
                .iter()
                .map(|ty| canonical_routine_type_name(&ty.sql_name()))
                .eq(signature.iter().cloned())
        });
        if same_builtin && !function.def.is_procedure {
            if engine.user_function_precedes_pg_catalog(&function.def.name) {
                builtins.retain(|builtin| {
                    !builtin
                        .argument_types
                        .iter()
                        .map(|ty| canonical_routine_type_name(&ty.sql_name()))
                        .eq(signature.iter().cloned())
                });
            } else {
                continue;
            }
        }
        visible_users.push(function);
    }

    let procedure_matches = visible_users.iter().any(|function| {
        function.def.is_procedure
            && static_function_match(function.clone(), argument_names, argument_types).is_some()
    });
    let mut candidates =
        visible_users
            .into_iter()
            .filter(|function| !function.def.is_procedure)
            .filter_map(|function| {
                let matched = static_function_match(function, argument_names, argument_types)?;
                Some(FunctionMatch {
                    target: FunctionTarget::User(matched.function),
                    argument_types: matched.argument_types,
                    exact_matches: matched.exact_matches,
                    preferred_matches: matched.preferred_matches,
                })
            })
            .chain(builtins.into_iter().filter_map(|builtin| {
                builtin_function_match(builtin, argument_names, argument_types)
            }))
            .collect::<Vec<_>>();
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

    retain_best_matches(&mut candidates);
    narrow_unknown_arguments(&mut candidates, argument_types);
    narrow_assumed_known_type(&mut candidates, argument_types);
    if candidates.len() != 1 {
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
            return_type: static_function_return_type(name, &function.def)?,
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

fn retain_best_matches(candidates: &mut Vec<FunctionMatch>) {
    let most_exact = candidates
        .iter()
        .map(|candidate| candidate.exact_matches)
        .max()
        .unwrap_or(0);
    candidates.retain(|candidate| candidate.exact_matches == most_exact);
    let most_preferred = candidates
        .iter()
        .map(|candidate| candidate.preferred_matches)
        .max()
        .unwrap_or(0);
    candidates.retain(|candidate| candidate.preferred_matches == most_preferred);
}

fn narrow_assumed_known_type(
    candidates: &mut Vec<FunctionMatch>,
    argument_types: &[Option<ColumnType>],
) {
    if candidates.len() <= 1 {
        return;
    }
    let mut known = argument_types.iter().flatten();
    let Some(first) = known.next() else {
        return;
    };
    let identity = canonical_column_type_name(first);
    if !known.all(|ty| canonical_column_type_name(ty) == identity) {
        return;
    }
    candidates.retain(|candidate| {
        argument_types.iter().enumerate().all(|(index, actual)| {
            actual.is_some()
                || routine_type_accepts_implicit_cast(&identity, &candidate.argument_types[index])
        })
    });
}

fn builtin_function_match(
    builtin: BuiltinFunctionOverload,
    argument_names: &[Option<String>],
    argument_types: &[Option<ColumnType>],
) -> Option<FunctionMatch> {
    if argument_types.len() != builtin.argument_types.len()
        || argument_names.len() != argument_types.len()
        || builtin.argument_names.len() != builtin.argument_types.len()
    {
        return None;
    }
    let mut slots = vec![None; builtin.argument_types.len()];
    let mut matched_argument_types = Vec::with_capacity(argument_types.len());
    let mut positional = 0usize;
    let mut saw_named = false;
    for (argument_name, argument_type) in argument_names.iter().zip(argument_types) {
        let index = if let Some(argument_name) = argument_name {
            saw_named = true;
            builtin
                .argument_names
                .iter()
                .position(|name| name.as_deref() == Some(argument_name.as_str()))?
        } else {
            if saw_named || positional >= builtin.argument_types.len() {
                return None;
            }
            let index = positional;
            positional += 1;
            index
        };
        if slots[index].replace(argument_type.as_ref()).is_some() {
            return None;
        }
        matched_argument_types.push(canonical_routine_type_name(
            &builtin.argument_types[index].sql_name(),
        ));
    }

    let mut exact_matches = 0usize;
    let mut preferred_matches = 0usize;
    for (actual, declared_type) in slots.into_iter().zip(&builtin.argument_types) {
        let actual_type = actual?;
        let Some(actual_type) = actual_type else {
            continue;
        };
        let declared = canonical_routine_type_name(&declared_type.sql_name());
        let actual = canonical_column_type_name(actual_type);
        if actual == declared {
            exact_matches += 1;
        } else if routine_type_accepts_implicit_cast(&actual, &declared) {
            preferred_matches += usize::from(routine_type_is_preferred(&declared));
        } else if let ColumnType::Domain { base, .. } = actual_type {
            let base = canonical_column_type_name(base);
            if !routine_type_accepts_implicit_cast(&base, &declared) {
                return None;
            }
            preferred_matches += usize::from(routine_type_is_preferred(&declared));
        } else {
            return None;
        }
    }
    Some(FunctionMatch {
        target: FunctionTarget::Builtin(builtin),
        argument_types: matched_argument_types,
        exact_matches,
        preferred_matches,
    })
}

fn narrow_unknown_arguments(
    candidates: &mut Vec<FunctionMatch>,
    argument_types: &[Option<ColumnType>],
) {
    for (index, actual) in argument_types.iter().enumerate() {
        if actual.is_some() || candidates.len() <= 1 {
            continue;
        }
        let mut categories = candidates
            .iter()
            .map(|candidate| routine_type_category(&candidate.argument_types[index]))
            .collect::<Vec<_>>();
        categories.sort_unstable();
        categories.dedup();
        let selected = if categories.contains(&'S') {
            Some('S')
        } else if categories.len() == 1 {
            categories.first().copied()
        } else {
            None
        };
        if let Some(selected) = selected {
            candidates.retain(|candidate| {
                routine_type_category(&candidate.argument_types[index]) == selected
            });
            if candidates
                .iter()
                .any(|candidate| routine_type_is_preferred(&candidate.argument_types[index]))
            {
                candidates.retain(|candidate| {
                    routine_type_is_preferred(&candidate.argument_types[index])
                });
            }
        }
    }
}

fn builtin_name_matches(name: &str, builtin_name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let builtin_name = builtin_name.to_ascii_lowercase();
    if name.contains('.') {
        name == builtin_name
    } else {
        builtin_name.rsplit('.').next() == Some(name.as_str())
    }
}

fn builtin_binding_matches(builtin: &BuiltinFunctionOverload, binding: &FunctionBinding) -> bool {
    builtin.name.eq_ignore_ascii_case(&binding.name)
        && builtin
            .argument_types
            .iter()
            .map(|ty| canonical_routine_type_name(&ty.sql_name()))
            .eq(binding
                .argument_types
                .iter()
                .map(|ty| canonical_routine_type_name(ty)))
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
