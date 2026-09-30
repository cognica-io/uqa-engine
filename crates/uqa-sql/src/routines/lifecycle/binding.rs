//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP and ALTER routine target binding and ownership validation.

use super::{
    alter_routine_kind_matches, alter_routine_kind_name, ambiguous_routine_error,
    ensure_routine_owner_as,
    names::{routine_lookup_keys, RoutineNameCatalog},
    routine_signature_display, wrong_routine_kind_error, RoutineDropResolution, RoutineDropTarget,
};
use crate::catalog::roles::identity::RoleSubject;
use crate::{
    ast::{AlterRoutineKind, DropFunctionItem, DropFunctionStmt},
    catalog::roles::{role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey},
    routines::{routine_signature_types, SQLUserFunction},
    SQLError,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub fn resolve_sql_function_drop_targets(
    catalog: &dyn RoutineNameCatalog,
    types: &dyn crate::routines::declaration::RoutineTypeCatalog,
    stmt: &DropFunctionStmt,
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    kind: &'static str,
) -> Result<RoutineDropResolution, SQLError> {
    let mut resolution = RoutineDropResolution {
        targets: Vec::new(),
        seen_targets: BTreeSet::new(),
        notices: Vec::new(),
    };
    for item in &stmt.items {
        // Argument types name catalog types, as `LookupFuncWithArgs` resolves them; a missing type skips the item under IF EXISTS.
        let requested_types = match crate::routines::declaration::resolve_routine_identity_types(
            types,
            item.arg_types.as_deref(),
            &[],
            "DROP routine",
        ) {
            Ok(types) => types,
            Err(error) if stmt.if_exists && error.sqlstate() == Some("42704") => {
                resolution
                    .notices
                    .push(crate::SQLNotice::notice(format!("{error}, skipping")));
                continue;
            }
            Err(error) => return Err(error),
        };
        let target = resolve_sql_function_drop_target(
            catalog,
            registry,
            item,
            requested_types.as_deref(),
            stmt.is_procedure,
            kind,
        )?;
        if let Some((key, position)) = target {
            let function = &registry[&key][position];
            let target = RoutineDropTarget {
                object_id: function.def.object_id,
                name: key,
                argument_types: routine_signature_types(&function.def),
                is_procedure: function.def.is_procedure,
            };
            if resolution.seen_targets.insert(target.clone()) {
                resolution.targets.push(target);
            }
        } else {
            let spelled = match &item.arg_types {
                Some(types) => format!("{}({})", item.name, types.join(", ")),
                None => format!("{}()", item.name),
            };
            if stmt.if_exists {
                resolution.notices.push(crate::SQLNotice::notice(format!(
                    "{kind} {spelled} does not exist, skipping"
                )));
                continue;
            }
            // The notice echoes the argument types as written; the error spells the resolved types as `format_type_be` does.
            let described = match requested_types.as_deref() {
                Some(types) => format!(
                    "{kind} {} does not exist",
                    routine_signature_display(catalog, &item.name, types)
                ),
                None => format!("could not find a {kind} named \"{}\"", item.name),
            };
            return Err(SQLError::Routine {
                sqlstate: "42883".into(),
                message: described,
            });
        }
    }
    Ok(resolution)
}

/// Find the routine a DROP item names; `requested_types` are its argument types resolved to catalog type names.
pub fn resolve_sql_function_drop_target(
    catalog: &dyn RoutineNameCatalog,
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    item: &DropFunctionItem,
    requested_types: Option<&[String]>,
    is_procedure: bool,
    expected_kind: &str,
) -> Result<Option<(String, usize)>, SQLError> {
    let keys = routine_lookup_keys(catalog, &item.name)?;
    // An argument list selects a routine of any kind, which then must be of the command's kind.
    if let Some(types) = requested_types {
        for key in keys {
            let Some(overloads) = registry.get(&key) else {
                continue;
            };
            let Some((position, function)) = overloads
                .iter()
                .enumerate()
                .find(|(_, function)| routine_signature_types(&function.def) == *types)
            else {
                continue;
            };
            if function.def.is_procedure != is_procedure {
                return Err(wrong_routine_kind_error(
                    &routine_signature_display(catalog, &item.name, types),
                    expected_kind,
                ));
            }
            return Ok(Some((key, position)));
        }
        return Ok(None);
    }
    // A bare name considers only routines of the command's kind, after search-path shadowing by declared identity.
    let mut visible_signatures = BTreeSet::new();
    let mut candidates = Vec::new();
    for key in keys {
        let Some(overloads) = registry.get(&key) else {
            continue;
        };
        for (position, function) in overloads.iter().enumerate() {
            if visible_signatures.insert(routine_signature_types(&function.def))
                && function.def.is_procedure == is_procedure
            {
                candidates.push((key.clone(), position));
            }
        }
    }
    match candidates.as_slice() {
        [] => Ok(None),
        [(key, position)] => Ok(Some((key.clone(), *position))),
        _ => Err(ambiguous_routine_error(expected_kind, &item.name)),
    }
}

pub fn resolve_sql_routine_alter_target(
    catalog: &dyn RoutineNameCatalog,
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    requested_name: &str,
    requested_types: Option<&[String]>,
    kind: AlterRoutineKind,
) -> Result<(String, usize), SQLError> {
    let kind_name = alter_routine_kind_name(kind);
    let keys = routine_lookup_keys(catalog, requested_name)?;
    if let Some(types) = requested_types {
        for key in keys {
            let Some(overloads) = registry.get(&key) else {
                continue;
            };
            let Some((position, function)) = overloads
                .iter()
                .enumerate()
                .find(|(_, function)| routine_signature_types(&function.def) == types)
            else {
                continue;
            };
            if !alter_routine_kind_matches(kind, &function.def) {
                return Err(wrong_routine_kind_error(
                    &routine_signature_display(catalog, requested_name, types),
                    kind_name,
                ));
            }
            return Ok((key, position));
        }
        return Err(SQLError::Routine {
            sqlstate: "42883".into(),
            message: format!(
                "{} {} does not exist",
                missing_routine_kind(kind),
                routine_signature_display(catalog, requested_name, types)
            ),
        });
    }

    // PostgreSQL applies search-path shadowing by declared identity before filtering FUNCTION versus PROCEDURE. Thus an earlier procedure can hide a same-signature function in a later schema, while a distinct later function remains visible.
    let mut visible_signatures = std::collections::BTreeSet::new();
    let mut candidates = Vec::new();
    for key in keys {
        let Some(overloads) = registry.get(&key) else {
            continue;
        };
        for (position, function) in overloads.iter().enumerate() {
            let signature = routine_signature_types(&function.def);
            if visible_signatures.insert(signature)
                && alter_routine_kind_matches(kind, &function.def)
            {
                candidates.push((key.clone(), position));
            }
        }
    }
    match candidates.as_slice() {
        [(name, position)] => Ok((name.clone(), *position)),
        [] => Err(SQLError::Routine {
            sqlstate: "42883".into(),
            message: format!(
                "could not find a {} named \"{requested_name}\"",
                missing_routine_kind(kind)
            ),
        }),
        _ => Err(ambiguous_routine_error(kind_name, requested_name)),
    }
}

/// `LookupFuncWithArgs` reports a missing `ROUTINE` as a missing function.
const fn missing_routine_kind(kind: AlterRoutineKind) -> &'static str {
    match kind {
        AlterRoutineKind::Procedure => "procedure",
        AlterRoutineKind::Function | AlterRoutineKind::Routine => "function",
    }
}

pub fn ensure_routine_drop_owners(
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    targets: &[RoutineDropTarget],
    current_user: &(impl RoleSubject + ?Sized),
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> Result<(), SQLError> {
    for target in targets {
        let definition = registry
            .get(&target.name)
            .and_then(|overloads| {
                overloads.iter().find(|function| {
                    function.def.is_procedure == target.is_procedure
                        && routine_signature_types(&function.def) == target.argument_types
                })
            })
            .map(|function| &function.def)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved {} {} disappeared before ownership validation",
                    target.kind(),
                    target.label()
                ))
            })?;
        ensure_routine_owner_as(
            definition,
            role_inherits(
                roles,
                memberships,
                current_user,
                &crate::routines::security::bound_routine_owner(definition)?,
            ),
        )?;
    }
    Ok(())
}
