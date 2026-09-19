//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain the selected routine and new role identities across ownership waits.

use super::RoutinePrivilegeContext;
use crate::{
    catalog::security::roles::{
        dependencies::{prepare_role_owner, RoleDependencyCandidate},
        locking::RoleLockContext,
    },
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
};
use std::sync::Arc;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::AlterRoutineOwnerStmt,
    catalog::roles::{require_set_role, resolve_role_specification, role_inherits},
    catalog::security::ownership::OwnerChangeAuthority,
    routines::{
        declaration::resolve_alter_routine_identity_types,
        lifecycle::{binding::resolve_sql_routine_alter_target, ensure_routine_owner_as},
        security as analysis, SQLUserFunction,
    },
    SQLError,
};

pub fn alter_sql_routine_owner(
    context: &RoutinePrivilegeContext<'_>,
    stmt: &AlterRoutineOwnerStmt,
) -> Result<(), SQLError> {
    let locks = RoleLockContext {
        roles: context.catalog.roles,
        session: context.locks,
    };
    let owner = locks.bind(&resolve_role_specification(
        context.role_names,
        &stmt.new_owner,
    ))?;
    let identity = analysis::routine_owner_identity(stmt);
    let requested_types = resolve_alter_routine_identity_types(context.types, &identity)?;
    let target = lock_owner_target(context, stmt, requested_types.as_deref())?;
    let current_user = context.catalog.names.current_role();
    let RoleDependencyCandidate {
        roles,
        memberships,
        value,
        ..
    } = prepare_role_owner(
        locks,
        &owner,
        || context.catalog.writer.prepare_writer(),
        |roles, memberships| {
            let registry = context.catalog.registry.routines_write();
            let (name, position, existing) = registry
                .iter()
                .find_map(|(name, overloads)| {
                    overloads
                        .iter()
                        .enumerate()
                        .find_map(|(position, routine)| {
                            (routine.def.object_id == target.def.object_id)
                                .then_some((name, position, routine))
                        })
                })
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42883".into(),
                    message: format!("routine {} does not exist", stmt.name),
                })?;
            let previous_owner = analysis::bound_routine_owner(&existing.def)?;
            if previous_owner == owner.identity() {
                return Ok(None);
            }
            ensure_routine_owner_as(
                &existing.def,
                role_inherits(roles, memberships, &current_user, &previous_owner),
            )?;
            require_set_role(roles, memberships, &current_user, &owner.name)?;
            let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
                SQLError::Internal(format!("resolve routine owner target: {error}"))
            })?;
            OwnerChangeAuthority {
                roles,
                memberships,
                current_user: &current_user,
                new_owner: &owner.name,
            }
            .require_schema_create(context.schemas, &relation.schema)?;
            let mut def = existing.def.clone();
            analysis::rewrite_routine_acl_owner(&mut def, previous_owner, owner.identity());
            def.owner = Some(owner.identity());
            let mut next = registry.clone();
            next.get_mut(name).expect("resolved routine key")[position] =
                Arc::new(SQLUserFunction {
                    def,
                    compiled: existing.compiled.clone(),
                });
            Ok(Some((registry, next)))
        },
    )?;
    let Some((mut registry, next)) = value else {
        return Ok(());
    };
    context
        .catalog
        .publication
        .persist_routine_definitions(&next)?;
    **registry = next;
    drop(registry);
    drop(memberships);
    drop(roles);
    context.catalog.changes.catalog_registry_changed();
    Ok(())
}

fn lock_owner_target(
    context: &RoutinePrivilegeContext<'_>,
    stmt: &AlterRoutineOwnerStmt,
    types: Option<&[String]>,
) -> Result<Arc<SQLUserFunction>, SQLError> {
    let resolve = || {
        let registry = context.catalog.registry.routine_snapshot();
        let (name, position) = resolve_sql_routine_alter_target(
            context.catalog.names,
            &registry,
            &stmt.name,
            types,
            stmt.kind,
        )?;
        Ok::<_, SQLError>(registry[&name][position].clone())
    };
    loop {
        let initial = resolve()?;
        let oid = crate::catalog::projection::user_routine_catalog_oid(&initial)?;
        let guard = context.locks.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id: 1255,
                oid: u32::try_from(oid)
                    .map_err(|_| SQLError::Internal("invalid routine OID".into()))?,
            },
            RelationLockMode::AccessExclusive,
        )?;
        context.locks.refresh_shared_catalog()?;
        let current = resolve()?;
        if initial.def.object_id == current.def.object_id {
            guard.retain();
            return Ok(current);
        }
    }
}
