//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select the catalog declaration of analyzed aggregate and window kernels.

use super::BuiltinRoutineIdentity;
use crate::{
    ColumnType, MatchedRoutineSignature, RankedFunctionMatch, RoutineCallDescriptor,
    RoutineParameterDescriptor, SQLError,
};

pub fn select_set_call(
    name: &str,
    arguments: &[Option<ColumnType>],
    window: bool,
    identities: impl Iterator<Item = BuiltinRoutineIdentity>,
) -> Result<Option<BuiltinRoutineIdentity>, SQLError> {
    let (schema, local) =
        uqa_core::RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
    if schema
        .as_deref()
        .is_some_and(|schema| schema != "pg_catalog")
    {
        return Ok(None);
    }
    let names = vec![None; arguments.len()];
    let mut candidates = Vec::new();
    for identity in identities.filter(|identity| {
        identity.name == local && identity.argument_types.len() == arguments.len()
    }) {
        // Analysis accepts each ordinary window kernel by name and arity; its declaration includes the polymorphic pseudo-types, not the concrete input types.
        if window && identity.kind == 'w' {
            return Ok(Some(identity));
        }
        if identity.kind != 'a' {
            continue;
        }
        let parameters = identity
            .argument_types
            .iter()
            .map(|oid| {
                let name = match oid {
                    700 => "real",
                    3500 => "anyenum",
                    _ => crate::catalog::type_metadata::catalog_type_name(*oid),
                };
                if name == "USER-DEFINED" {
                    return Err(SQLError::Internal(format!(
                        "aggregate declaration has unknown type OID {oid}"
                    )));
                }
                Ok(RoutineParameterDescriptor {
                    name: None,
                    type_name: name.into(),
                    column_type: ColumnType::from_sql_name(name).ok(),
                    has_default: false,
                    default_type: None,
                    variadic: false,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let call = RoutineCallDescriptor {
            argument_names: &names,
            argument_types: arguments,
            explicit_variadic: false,
        };
        if let Some(signature) = crate::type_resolution::match_routine_candidate(&parameters, call)
            .map_err(|error| {
                SQLError::Internal(format!("analyzed aggregate signature: {error:?}"))
            })?
        {
            candidates.push(Candidate {
                identity,
                signature,
            });
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    if !crate::rank_function_matches(&mut candidates, arguments) || candidates.len() != 1 {
        return Err(crate::function_resolution_error(
            "42725",
            name,
            &names,
            arguments,
            "is not unique",
        ));
    }
    Ok(Some(candidates[0].identity))
}

struct Candidate {
    identity: BuiltinRoutineIdentity,
    signature: MatchedRoutineSignature,
}
impl RankedFunctionMatch for Candidate {
    fn argument_types(&self) -> &[String] {
        self.signature.argument_types()
    }
    fn raw_exact_matches(&self) -> usize {
        self.signature.raw_exact_matches
    }
    fn exact_matches(&self) -> usize {
        self.signature.exact_matches
    }
    fn preferred_matches(&self) -> usize {
        self.signature.preferred_matches
    }
}
