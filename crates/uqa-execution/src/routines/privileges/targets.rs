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
    routines::{
        lifecycle::{binding::resolve_sql_routine_alter_target, RoutineRegistry},
        security::grants,
        SQLUserFunction,
    },
    SQLError,
};

pub(super) type RoutineGrantTargets = Vec<Arc<SQLUserFunction>>;

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
    let snapshot = context.catalog.registry.routine_snapshot();
    let selected = if let Some(schemas) = &stmt.schemas {
        if !stmt.items.is_empty() {
            return Err(SQLError::Internal(
                "routine privilege declaration mixes explicit and schema targets".into(),
            ));
        }
        grants::routines_in_schemas(
            context.catalog.names,
            &snapshot,
            schemas,
            stmt.kind,
            user_routine_catalog_oid,
        )?
    } else {
        stmt.items
            .iter()
            .map(|item| {
                let requested_types =
                    uqa_sql::routines::declaration::resolve_routine_identity_types(
                        context.types,
                        item.arg_types.as_deref(),
                        &[],
                        "GRANT routine",
                    )?;
                resolve_sql_routine_alter_target(
                    context.catalog.names,
                    &snapshot,
                    &item.name,
                    requested_types.as_deref(),
                    stmt.kind,
                )
            })
            .collect::<Result<Vec<_>, SQLError>>()?
    };
    selected
        .into_iter()
        .map(|(name, position)| {
            let routine = &snapshot[&name][position];
            if routine.def.object_id.is_none() {
                return Err(SQLError::Internal(format!(
                    "routine `{name}` has no catalog object identity"
                )));
            }
            Ok(Arc::clone(routine))
        })
        .collect()
}

/// Resolve the captured identities in one registry pass, preserving the command's repeated targets and written order.
pub(super) fn current(
    targets: &[Arc<SQLUserFunction>],
    registry: &RoutineRegistry,
) -> Result<Vec<(String, usize)>, SQLError> {
    let requested: std::collections::BTreeSet<_> = targets
        .iter()
        .filter_map(|target| target.def.object_id)
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
            target
                .def
                .object_id
                .and_then(|identity| positions.get(&identity))
                .cloned()
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
