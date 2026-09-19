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
        definition, dependencies::ensure_roles_have_no_object_dependencies, identity::RoleBinding,
        tuple::RoleTuple, RoleReference,
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
    bindings: &[RoleBinding],
    current: &RoleReference,
    session: &RoleReference,
) -> Result<Vec<RoleTuple>, SQLError> {
    let roles = context.analysis.roles.role_definitions();
    let names = bindings
        .iter()
        .map(|bound| bound.require_name(&roles).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    for name in &names {
        definition::require_role_drop_authority(&context.analysis, &roles, current, session, name)?;
    }
    let identities = names
        .iter()
        .map(|name| roles[name].identity())
        .collect::<BTreeSet<_>>();
    let memberships = context.analysis.roles.role_memberships();
    definition::ensure_no_grantor_dependencies(&memberships, &identities)?;
    ensure_roles_have_no_object_dependencies(context.dependencies, &names, &roles)?;
    let mut targets = Vec::with_capacity(names.len());
    for name in &names {
        let target = RoleTuple::bind(&roles[name])?;
        if context
            .temporary_roles
            .peer_temporary_role_reference(target.role.oid)?
        {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{name}\" cannot be dropped because some objects depend on it"
                ),
            });
        }
        targets.push(target);
    }
    Ok(targets)
}
