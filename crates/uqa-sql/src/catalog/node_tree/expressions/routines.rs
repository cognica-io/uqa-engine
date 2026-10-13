//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind catalog declarations through the shared routine matcher and overload ranking.

use super::{ColumnType, RoutineIdentity, SQLError};
use crate::ast::RoutineVariadicMode;
use crate::type_resolution::{
    function_resolution_error, match_routine_signature, rank_function_matches,
    MatchedRoutineSignature, RankedFunctionMatch, RoutineCallDescriptor,
    RoutineParameterDescriptor,
};

pub struct RoutineDeclaration {
    pub oid: i64,
    pub parameters: Vec<RoutineParameterDescriptor>,
    pub return_type: String,
}

struct Candidate<'a> {
    declaration: &'a RoutineDeclaration,
    matched: MatchedRoutineSignature,
    wildcard: Option<usize>,
}

impl RankedFunctionMatch for Candidate<'_> {
    fn argument_types(&self) -> &[String] {
        self.matched.argument_types()
    }
    fn raw_exact_matches(&self) -> usize {
        self.matched.raw_exact_matches()
    }
    fn exact_matches(&self) -> usize {
        self.matched.exact_matches()
    }
    fn preferred_matches(&self) -> usize {
        self.matched.preferred_matches()
    }
    fn is_variadic_expansion(&self) -> bool {
        self.matched.is_variadic_expansion()
    }
}

/// Select a declared catalog signature while retaining supplied positions and concrete polymorphic targets.
pub fn select_routine(
    name: &str,
    declarations: &[RoutineDeclaration],
    call: RoutineCallDescriptor<'_>,
) -> Result<RoutineIdentity, SQLError> {
    let mut candidates = Vec::new();
    let mut failure = None;
    for declaration in declarations {
        match candidate(name, declaration, call) {
            Ok(Some(candidate)) => candidates.push(candidate),
            Ok(None) => {}
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }

    if candidates.is_empty() {
        if let Some(error) = failure {
            return Err(error);
        }
        return Err(function_resolution_error(
            "42883",
            name,
            call.argument_names,
            call.argument_types,
            "does not exist",
        ));
    }
    if !rank_function_matches(&mut candidates, call.argument_types) || candidates.len() != 1 {
        return Err(function_resolution_error(
            "42725",
            name,
            call.argument_names,
            call.argument_types,
            "is not unique",
        ));
    }
    let Candidate {
        declaration,
        matched,
        wildcard,
    } = candidates.pop().expect("one selected catalog routine");
    let result_type = matched
        .substitute_type(&declaration.return_type)
        .ok_or_else(|| SQLError::Internal("unresolved catalog routine result type".into()))?;
    let mut invocation = matched.invocation_binding(Some(&declaration.return_type));
    if let Some(position) = wildcard {
        invocation.parameter_types.truncate(position);
        invocation.parameter_types.push("any".into());
        for index in position..call.argument_types.len() {
            invocation.argument_positions[index] = position;
            invocation.argument_targets[index] = call.argument_types[index]
                .as_ref()
                .map_or_else(|| "unknown".into(), ColumnType::catalog_name);
        }
        invocation.variadic_mode = if call.explicit_variadic {
            RoutineVariadicMode::Explicit {
                parameter_index: position,
            }
        } else {
            RoutineVariadicMode::Expanded {
                parameter_index: position,
            }
        };
    }
    Ok(RoutineIdentity {
        oid: declaration.oid,
        invocation,
        result_type,
    })
}

fn candidate<'a>(
    name: &str,
    declaration: &'a RoutineDeclaration,
    call: RoutineCallDescriptor<'_>,
) -> Result<Option<Candidate<'a>>, SQLError> {
    let wildcard = declaration
        .parameters
        .last()
        .filter(|parameter| parameter.variadic && parameter.type_name == "any")
        .map(|_| declaration.parameters.len() - 1);
    let parameters;
    let matching = if let Some(position) = wildcard {
        if call.argument_types.len() <= position || call.argument_names.iter().any(Option::is_some)
        {
            return Ok(None);
        }
        if call.explicit_variadic
            && (call.argument_types.len() != position + 1
                || !matches!(call.argument_types[position], Some(ColumnType::Array(_))))
        {
            return Ok(None);
        }
        // VARIADIC any consumes individually typed values, without selecting a common element type or forming an array.
        parameters = declaration.parameters[..position]
            .iter()
            .cloned()
            .chain(call.argument_types[position..].iter().map(|ty| {
                let ty = ty.clone().unwrap_or(ColumnType::Text);
                RoutineParameterDescriptor {
                    name: None,
                    type_name: ty.catalog_name(),
                    column_type: Some(ty),
                    has_default: false,
                    default_type: None,
                    variadic: false,
                }
            }))
            .collect::<Vec<_>>();
        &parameters
    } else {
        &declaration.parameters
    };
    let mut matching_call = call;
    if wildcard.is_some() {
        matching_call.explicit_variadic = false;
    }
    let matched = match_routine_signature(matching, matching_call).map_err(|error| {
        crate::routines::resolution::static_signature_error(
            crate::routines::resolution::RoutineCallKind::Function,
            name,
            error,
        )
    })?;
    Ok(matched.map(|matched| Candidate {
        declaration,
        matched,
        wildcard,
    }))
}
