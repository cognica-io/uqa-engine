//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP and ALTER routine target binding and ownership validation.

use super::{
    alter_routine_kind_matches, alter_routine_kind_name, ensure_routine_owner_as,
    names::{routine_lookup_keys, RoutineNameCatalog},
    routine_signature_label, wrong_routine_kind_error, RoutineDropResolution, RoutineDropTarget,
};
use crate::{
    ast::{AlterRoutineKind, DropFunctionItem, DropFunctionStmt},
    catalog::roles::{role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey},
    routines::{routine_signature_types, SQLUserFunction},
    type_resolution::canonical_routine_type_name,
    SQLError,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub fn resolve_sql_function_drop_targets(
    catalog: &dyn RoutineNameCatalog,
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
        let target =
            resolve_sql_function_drop_target(catalog, registry, item, stmt.is_procedure, kind)?;
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
                resolution.notices.push((
                    "NOTICE",
                    format!("{kind} {spelled} does not exist, skipping"),
                ));
                continue;
            }
            let described = match &item.arg_types {
                Some(_) => format!("{kind} {spelled} does not exist"),
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

pub fn resolve_sql_function_drop_target(
    catalog: &dyn RoutineNameCatalog,
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    item: &DropFunctionItem,
    is_procedure: bool,
    expected_kind: &str,
) -> Result<Option<(String, usize)>, SQLError> {
    let requested_types = item.arg_types.as_ref().map(|types| {
        types
            .iter()
            .map(|type_name| canonical_routine_type_name(type_name))
            .collect::<Vec<_>>()
    });
    for key in routine_lookup_keys(catalog, &item.name)? {
        let Some(overloads) = registry.get(&key) else {
            continue;
        };
        if let Some(types) = requested_types.as_ref() {
            let Some((position, function)) = overloads
                .iter()
                .enumerate()
                .find(|(_, function)| routine_signature_types(&function.def) == *types)
            else {
                continue;
            };
            if function.def.is_procedure != is_procedure {
                return Err(wrong_routine_kind_error(
                    &function.def.name,
                    types,
                    function.def.is_procedure,
                    expected_kind,
                ));
            }
            return Ok(Some((key, position)));
        }

        let positions = overloads
            .iter()
            .enumerate()
            .filter(|(_, function)| function.def.is_procedure == is_procedure)
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        match positions.as_slice() {
            [] => {
                if let Some(function) = overloads.first() {
                    return Err(wrong_routine_kind_error(
                        &function.def.name,
                        &routine_signature_types(&function.def),
                        function.def.is_procedure,
                        expected_kind,
                    ));
                }
            }
            [position] => return Ok(Some((key, *position))),
            _ => {
                return Err(SQLError::Routine {
                    sqlstate: "42725".into(),
                    message: format!("{expected_kind} name \"{}\" is not unique", item.name),
                });
            }
        }
    }
    Ok(None)
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
                    &function.def.name,
                    types,
                    function.def.is_procedure,
                    kind_name,
                ));
            }
            return Ok((key, position));
        }
        return Err(SQLError::Routine {
            sqlstate: "42883".into(),
            message: format!(
                "{kind_name} {} does not exist",
                routine_signature_label(requested_name, types)
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
            message: format!("could not find a {kind_name} named \"{requested_name}\""),
        }),
        _ => Err(SQLError::Routine {
            sqlstate: "42725".into(),
            message: format!("{kind_name} name \"{requested_name}\" is not unique"),
        }),
    }
}

pub fn ensure_routine_drop_owners(
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    targets: &[RoutineDropTarget],
    current_user: &str,
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
            role_inherits(roles, memberships, current_user, &definition.owner),
        )?;
    }
    Ok(())
}
