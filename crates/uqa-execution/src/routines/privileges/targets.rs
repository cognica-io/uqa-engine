//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind the whole command before mutation and retain those routine identities across writer and role waits.

use super::RoutinePrivilegeContext;
use crate::catalog::projection::user_routine_catalog_oid;
use std::sync::Arc;
use uqa_sql::{
    ast::GrantRoutineStmt,
    catalog::roles::resolve_role_specification,
    routines::{lifecycle::RoutineRegistry, security::grants, SQLUserFunction},
    SQLError,
};

#[derive(Clone)]
pub(super) enum RoutineGrantTarget {
    User(Arc<SQLUserFunction>),
    Builtin(crate::catalog::projection::BuiltinRoutineIdentity),
}
pub(super) type RoutineGrantTargets = Vec<RoutineGrantTarget>;

pub(super) fn bind(
    context: &RoutinePrivilegeContext<'_>,
    stmt: &GrantRoutineStmt,
) -> Result<RoutineGrantTargets, SQLError> {
    if let Some(grantor) = &stmt.grantor {
        let roles = context.catalog.roles.role_definitions();
        let requested =
            resolve_role_specification(context.role_names, grantor).catalog_name(&roles)?;
        grants::validate_grantor(Some(&requested), &context.role_names.current_role(), &roles)?;
    }
    if stmt.schemas.is_some() && !stmt.items.is_empty() {
        return Err(SQLError::Internal(
            "routine privilege declaration mixes explicit and schema targets".into(),
        ));
    }
    let snapshot = context.catalog.registry.routine_snapshot();
    let mut targets = Vec::new();
    let mut identities = Vec::new();
    for (name, overloads) in &snapshot {
        for routine in overloads {
            let oid = u32::try_from(user_routine_catalog_oid(routine)?)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            let relation = uqa_core::RelationIdentity::from_legacy_name(name)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            let argument_types = uqa_sql::routines::routine_signature_types(&routine.def)
                .iter()
                .map(|ty| {
                    crate::catalog::projection::catalog_routine_type_oid(&context.snapshot, ty)
                })
                .collect();
            identities.push(grants::targets::RoutinePrivilegeIdentity {
                oid,
                relation,
                argument_types,
                kind: if routine.def.is_procedure { 'p' } else { 'f' },
            });
            targets.push(RoutineGrantTarget::User(Arc::clone(routine)));
        }
    }
    for routine in crate::catalog::projection::builtin_routine_identities() {
        identities.push(grants::targets::RoutinePrivilegeIdentity {
            oid: routine.oid,
            relation: uqa_core::RelationIdentity::new("pg_catalog", routine.name),
            argument_types: routine.argument_types.to_vec(),
            kind: routine.kind,
        });
        targets.push(RoutineGrantTarget::Builtin(routine));
    }
    let selected = if let Some(schemas) = &stmt.schemas {
        grants::targets::in_schemas(context.catalog.names, &identities, schemas, stmt.kind)?
    } else {
        stmt.items
            .iter()
            .map(|item| {
                let types = uqa_sql::routines::declaration::resolve_routine_identity_types(
                    context.types,
                    item.arg_types.as_deref(),
                    &[],
                    "GRANT routine",
                )?;
                let oids = types.as_ref().map(|types| {
                    types
                        .iter()
                        .map(|ty| {
                            crate::catalog::projection::catalog_routine_type_oid(
                                &context.snapshot,
                                ty,
                            )
                        })
                        .collect::<Vec<_>>()
                });
                grants::targets::named(
                    context.catalog.names,
                    &identities,
                    &item.name,
                    oids.as_deref(),
                    types.as_deref(),
                    stmt.kind,
                )
            })
            .collect::<Result<Vec<_>, SQLError>>()?
    };
    Ok(selected
        .into_iter()
        .map(|index| targets[index].clone())
        .collect())
}

pub(super) enum CurrentRoutineGrantTarget {
    User(String, usize),
    Builtin(crate::catalog::projection::BuiltinRoutineIdentity),
}

/// Resolve the captured identities in one registry pass, preserving the command's repeated targets and written order.
pub(super) fn current(
    targets: &[RoutineGrantTarget],
    registry: &RoutineRegistry,
) -> Result<Vec<CurrentRoutineGrantTarget>, SQLError> {
    let requested: std::collections::BTreeSet<_> = targets
        .iter()
        .filter_map(|target| match target {
            RoutineGrantTarget::User(target) => target.def.object_id,
            RoutineGrantTarget::Builtin(_) => None,
        })
        .collect();
    let mut positions = std::collections::BTreeMap::new();
    for (name, overloads) in registry {
        for (position, routine) in overloads.iter().enumerate() {
            if let Some(identity) = routine.def.object_id {
                if requested.contains(&identity) {
                    positions.insert(identity, (name.clone(), position));
                }
            }
        }
    }
    targets
        .iter()
        .map(|target| {
            let target = match target {
                RoutineGrantTarget::Builtin(target) => {
                    return Ok(CurrentRoutineGrantTarget::Builtin(*target))
                }
                RoutineGrantTarget::User(target) => target,
            };
            target
                .def
                .object_id
                .and_then(|identity| positions.get(&identity))
                .map(|(name, position)| CurrentRoutineGrantTarget::User(name.clone(), *position))
                .ok_or_else(|| match user_routine_catalog_oid(target) {
                    Ok(oid) => SQLError::Routine {
                        sqlstate: "XX000".into(),
                        message: format!("cache lookup failed for function {oid}"),
                    },
                    Err(error) => error,
                })
        })
        .collect()
}
