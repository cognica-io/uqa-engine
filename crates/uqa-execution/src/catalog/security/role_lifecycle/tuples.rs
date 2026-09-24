//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serialize role definition tuples without excluding shared authority dependencies.

use super::context::RoleExecutionContext;
use crate::{
    catalog::security::roles::locking::ROLE_CATALOG_CLASS_ID,
    row_locks::{shared_objects::SharedCatalogLock, RelationLockMode},
};
use std::collections::BTreeSet;
use uqa_sql::{
    catalog::roles::{
        definition,
        dependencies::ensure_roles_have_no_object_dependencies,
        identity::{RoleBinding, RoleSubject},
        tuple::RoleTuple,
    },
    SQLError,
};

pub(super) fn lock(
    context: &RoleExecutionContext<'_>,
    original: &RoleTuple,
) -> Result<(), SQLError> {
    let guard = context.locks.acquire_shared_catalog(
        SharedCatalogLock::Tuple {
            class_id: ROLE_CATALOG_CLASS_ID,
            oid: original.role.oid,
        },
        RelationLockMode::AccessExclusive,
    )?;
    context.locks.refresh_shared_catalog()?;
    original.revalidate(&context.analysis.roles.role_definitions())?;
    guard.retain();
    Ok(())
}

pub(super) fn prepare_drop(
    context: &RoleExecutionContext<'_>,
    bound: &RoleBinding,
) -> Result<RoleTuple, SQLError> {
    let roles = context.analysis.roles.role_definitions();
    let role = bound
        .role_definition(&roles)
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "XX000".into(),
            message: format!("could not find tuple for role {}", bound.oid),
        })?;
    let identities = BTreeSet::from([role.identity()]);
    let memberships = context.analysis.roles.role_memberships();
    definition::ensure_no_grantor_dependencies(&memberships, &identities)?;
    ensure_roles_have_no_object_dependencies(
        context.dependencies,
        std::slice::from_ref(&role.name),
        &roles,
    )?;
    if context
        .temporary_roles
        .peer_temporary_role_reference(bound.oid)?
    {
        return Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: format!(
                "role \"{}\" cannot be dropped because some objects depend on it",
                role.name,
            ),
        });
    }
    RoleTuple::bind(role)
}
