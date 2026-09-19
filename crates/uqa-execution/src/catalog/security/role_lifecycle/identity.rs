//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocate public role OIDs independently from durable role incarnations.

use super::super::roles::locking::ROLE_CATALOG_CLASS_ID;
use super::context::RoleExecutionContext;
use crate::row_locks::{shared_objects::SharedCatalogLock, RelationLockMode};
use uqa_sql::{catalog::roles::RoleDefinition, SQLError};

pub(super) const MEMBERSHIP_CATALOG_CLASS_ID: u32 = 1261;

pub(super) fn reserve_membership_oid(
    context: &RoleExecutionContext<'_>,
    staged: &std::collections::BTreeSet<i64>,
    mut allocate: impl FnMut() -> Result<i64, SQLError>,
) -> Result<i64, SQLError> {
    let assigned = |oid| {
        let _roles = context.analysis.roles.role_definitions();
        let memberships = context.analysis.roles.role_memberships();
        staged.contains(&oid) || memberships.values().any(|membership| membership.oid == oid)
    };
    loop {
        let oid = allocate()?;
        let public_oid = u32::try_from(oid)
            .ok()
            .filter(|oid| *oid >= 16_384)
            .ok_or_else(|| SQLError::Internal("invalid role membership OID allocation".into()))?;
        if assigned(oid) {
            continue;
        }
        let guard = context.locks.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id: MEMBERSHIP_CATALOG_CLASS_ID,
                oid: public_oid,
            },
            RelationLockMode::AccessExclusive,
        )?;
        context.locks.refresh_shared_catalog()?;
        if assigned(oid) {
            continue;
        }
        guard.retain();
        return Ok(oid);
    }
}

pub(super) fn allocate_oid() -> Result<i64, SQLError> {
    crate::catalog::identity::allocate_catalog_oid("role")
}

pub(super) fn reserve_definition(
    context: &RoleExecutionContext<'_>,
    statement: &uqa_sql::ast::CreateRoleStmt,
) -> Result<RoleDefinition, SQLError> {
    if context
        .analysis
        .roles
        .role_definitions()
        .contains_key(&statement.name)
    {
        return Err(SQLError::Routine {
            sqlstate: "42710".into(),
            message: format!("role \"{}\" already exists", statement.name),
        });
    }
    let name_guard = context.locks.acquire_shared_catalog(
        SharedCatalogLock::Name {
            class_id: ROLE_CATALOG_CLASS_ID,
            name: &statement.name,
        },
        RelationLockMode::AccessExclusive,
    )?;
    context.locks.refresh_shared_catalog()?;
    if context
        .analysis
        .roles
        .role_definitions()
        .contains_key(&statement.name)
    {
        return Err(SQLError::Routine {
            sqlstate: "23505".into(),
            message: "duplicate key value violates unique constraint \"pg_authid_rolname_index\""
                .into(),
        });
    }
    name_guard.retain();
    reserve_oid(context, statement, allocate_oid)
}

pub(super) fn reserve_oid(
    context: &RoleExecutionContext<'_>,
    statement: &uqa_sql::ast::CreateRoleStmt,
    mut allocate: impl FnMut() -> Result<i64, SQLError>,
) -> Result<RoleDefinition, SQLError> {
    let oid = loop {
        let oid = allocate()?;
        if context
            .analysis
            .roles
            .role_definitions()
            .values()
            .any(|role| role.oid == oid)
        {
            continue;
        }
        let guard = context.locks.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id: ROLE_CATALOG_CLASS_ID,
                oid: u32::try_from(oid)
                    .map_err(|_| SQLError::Internal("invalid role OID allocation".into()))?,
            },
            RelationLockMode::AccessExclusive,
        )?;
        context.locks.refresh_shared_catalog()?;
        if context
            .analysis
            .roles
            .role_definitions()
            .values()
            .any(|role| role.oid == oid)
        {
            continue;
        }
        guard.retain();
        break oid;
    };
    let object_id = crate::catalog::identity::new_nonzero_catalog_identity("role", "identity")
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    Ok(RoleDefinition::from_create(statement, oid, object_id))
}
