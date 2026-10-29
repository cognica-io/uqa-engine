//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Builtin and catalog candidates share matching and ranking while borrowing their declarations.

use super::{diagnostics, matching::RoutineMatch, selection};
use crate::{
    ast::FunctionBinding,
    routines::resolution::{RoutineCallKind, RoutineOverloadContext},
    type_resolution::{
        builtin_name_matches, match_builtin_with_control, rank_function_matches_with_control,
        resolve_local_builtin_call_with_control, BuiltinFunctionOverload, Candidates,
        MatchedFunctionSignature, RankedFunctionMatch, RankingCandidates, ResolvedFunctionOverload,
        RoutineCallDescriptor,
    },
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

enum Candidate<'a> {
    User(Produced<Box<RoutineMatch>>),
    Builtin {
        declaration: &'a BuiltinFunctionOverload,
        signature: Produced<MatchedFunctionSignature>,
    },
}

impl RankedFunctionMatch for Candidate<'_> {
    fn argument_types(&self) -> &[String] {
        match self {
            Self::User(candidate) => candidate.argument_types(),
            Self::Builtin { signature, .. } => &signature.argument_types,
        }
    }
    fn raw_exact_matches(&self) -> usize {
        match self {
            Self::User(candidate) => candidate.raw_exact_matches(),
            Self::Builtin { signature, .. } => signature.raw_exact_matches,
        }
    }
    fn exact_matches(&self) -> usize {
        match self {
            Self::User(candidate) => candidate.exact_matches(),
            Self::Builtin { signature, .. } => signature.exact_matches,
        }
    }
    fn preferred_matches(&self) -> usize {
        match self {
            Self::User(candidate) => candidate.preferred_matches(),
            Self::Builtin { signature, .. } => signature.preferred_matches,
        }
    }
}

pub(in crate::routines::resolution) fn resolve(
    resolver: &RoutineOverloadContext<'_>,
    name: &str,
    binding: Option<&FunctionBinding>,
    call: RoutineCallDescriptor<'_>,
    builtins: &[BuiltinFunctionOverload],
    control: &ProductionControl<'_>,
) -> Result<Produced<ResolvedFunctionOverload>, SQLError> {
    control.check()?;
    if let Some(binding) = binding {
        return if binding.builtin {
            resolve_local_builtin_call_with_control(name, Some(binding), call, builtins, control)
        } else {
            selection::resolve(resolver, name, Some(binding), call, control)?.ok_or_else(|| {
                diagnostics::bound(RoutineCallKind::Function, binding, control)
                    .unwrap_or_else(std::convert::identity)
            })
        };
    }
    let overloads = resolver
        .catalog
        .lookup_sql_routine_candidates_with_control(name, control)?;
    let (mut users, match_error) = selection::collect(
        resolver,
        overloads.as_deref().map_or(&[], Vec::as_slice),
        call,
        control,
    )?;
    drop(overloads);
    let mut builtin_candidates = Candidates::new(control);
    for builtin in builtins {
        control.check()?;
        if builtin_name_matches(name, &builtin.name) {
            if let Some(signature) = match_builtin_with_control(builtin, call, control)? {
                builtin_candidates.push(Candidate::Builtin {
                    declaration: builtin,
                    signature,
                })?;
            }
        }
    }
    let mut builtin_candidates = builtin_candidates.finish();
    shadow(
        resolver,
        &mut users,
        &mut builtin_candidates,
        call.explicit_variadic,
        control,
    )?;
    let mut candidates = Candidates::new(control);
    for candidate in users.values.drain(..) {
        control.check()?;
        let allocation = control.reserve(size_of::<RoutineMatch>())?;
        candidates.push(Candidate::User(
            control.finish(Box::new(candidate), allocation)?,
        ))?;
    }
    drop(users);
    for candidate in builtin_candidates.values.drain(..) {
        control.check()?;
        candidates.push(candidate)?;
    }
    drop(builtin_candidates);
    let mut candidates = candidates.finish();
    if candidates.values.is_empty() {
        if let Some(error) = match_error {
            return Err(diagnostics::signature(&error, control)?);
        }
        return Err(diagnostics::resolution(
            RoutineCallKind::Function,
            "42883",
            name,
            call,
            "does not exist",
            control,
        )?);
    }
    drop(match_error);
    if !rank_function_matches_with_control(&mut candidates.values, call.argument_types, control)?
        || candidates.values.len() != 1
    {
        return Err(diagnostics::resolution(
            RoutineCallKind::Function,
            "42725",
            name,
            call,
            "is not unique",
            control,
        )?);
    }
    let selected = candidates.values.pop().expect("one selected routine");
    drop(candidates);
    resolve_selected(resolver, name, call, selected, control)
}

fn resolve_selected(
    resolver: &RoutineOverloadContext<'_>,
    name: &str,
    call: RoutineCallDescriptor<'_>,
    selected: Candidate<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ResolvedFunctionOverload>, SQLError> {
    match selected {
        Candidate::User(selected) => {
            let (selected, allocation) = selected.into_parts();
            let selected = *selected;
            drop(allocation);
            selection::resolved(resolver, name, call, selected, control)
        }
        Candidate::Builtin {
            declaration,
            signature,
        } => {
            drop(signature);
            resolve_local_builtin_call_with_control(
                &declaration.name,
                None,
                call,
                std::slice::from_ref(declaration),
                control,
            )
        }
    }
}

fn shadow(
    resolver: &RoutineOverloadContext<'_>,
    users: &mut RankingCandidates<RoutineMatch>,
    builtins: &mut RankingCandidates<Candidate<'_>>,
    explicit_variadic: bool,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut precedes = ProductionVec::new(*control);
    for candidate in &users.values {
        control.check()?;
        let mut precedence = resolver.user_function_precedes_pg_catalog_with_control(
            &candidate.function.def.name,
            control,
        )?;
        if !precedence
            && !explicit_variadic
            && candidate.function.def.name.starts_with("pg_catalog.")
            && matches!(
                candidate.invocation.variadic_mode,
                crate::ast::RoutineVariadicMode::None
            )
        {
            for builtin in &builtins.values {
                control.check()?;
                if builtin.argument_types() == candidate.argument_types()
                    && matches!(builtin, Candidate::Builtin { declaration, .. } if declaration.variadic)
                {
                    precedence = true;
                    break;
                }
            }
        }
        precedes.push_copy(precedence)?;
    }
    let precedes = precedes.finish()?;
    // Both decisions observe the original candidate sets: removing builtins first would lose the signatures that mask later user candidates.
    let mut keep_users = ProductionVec::new(*control);
    let mut keep_builtins = ProductionVec::new(*control);
    for user in &users.values {
        let has_preceding =
            preceding_signature(&users.values, &precedes, user.argument_types(), control)?;
        let mut has_builtin = false;
        for builtin in &builtins.values {
            control.check()?;
            if builtin.argument_types() == user.argument_types() {
                has_builtin = true;
                break;
            }
        }
        keep_users.push_copy(has_preceding || !has_builtin)?;
    }
    for builtin in &builtins.values {
        keep_builtins.push_copy(!preceding_signature(
            &users.values,
            &precedes,
            builtin.argument_types(),
            control,
        )?)?;
    }
    retain_mask(&mut users.values, &keep_users.finish()?, control)?;
    retain_mask(&mut builtins.values, &keep_builtins.finish()?, control)
}

fn preceding_signature(
    users: &[RoutineMatch],
    precedes: &[bool],
    signature: &[String],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    for (user, precedes) in users.iter().zip(precedes) {
        control.check()?;
        if *precedes && user.argument_types() == signature {
            return Ok(true);
        }
    }
    Ok(false)
}

fn retain_mask<T>(
    values: &mut Vec<T>,
    mask: &[bool],
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut position = 0;
    for keep in mask {
        control.check()?;
        if *keep {
            position += 1;
        } else {
            values.remove(position);
        }
    }
    Ok(())
}
