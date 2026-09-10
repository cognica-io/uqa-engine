//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared relation rename publication and role-transfer authorization boundaries.
use crate::catalog::security::roles::RoleCatalogGuards;
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::RoleAttribute,
    catalog::roles::{self, RoleReferenceNames},
    SQLError,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub trait RelationAlterLocks {
    fn lock_exclusive(&self, name: &str) -> Result<(), SQLError>;
}

pub trait RelationRenameDependencies {
    fn rewrite_views(
        &self,
        renames: &BTreeMap<RelationIdentity, RelationIdentity>,
    ) -> StorageBackendResult<()>;
    fn rewrite_routines(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> Result<(), String>;
    fn rename_events(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<()>;
}

pub fn rewrite_relation_rename_dependents(
    dependencies: &dyn RelationRenameDependencies,
    from: &RelationIdentity,
    to: &RelationIdentity,
) -> StorageBackendResult<()> {
    dependencies.rewrite_views(&BTreeMap::from([(from.clone(), to.clone())]))?;
    dependencies
        .rewrite_routines(from, to)
        .map_err(StorageBackendError::Other)?;
    dependencies.rename_events(from, to)
}

pub trait RoleTargetSchemaAccess {
    fn require_schema_create(&self, schema: &str, role: &str) -> Result<(), SQLError>;
}

pub struct RoleTransferContext<'a> {
    pub roles: &'a dyn RoleCatalogGuards,
    pub session: &'a dyn RoleReferenceNames,
    pub schemas: &'a dyn RoleTargetSchemaAccess,
}

/// Validate the target while retaining the original roles-then-memberships guard order.
pub fn role_transfer_target(
    context: &RoleTransferContext<'_>,
    requested_owner: &str,
) -> Result<(String, bool), SQLError> {
    let new_owner = roles::resolve_role_reference(context.session, requested_owner);
    let current_user_is_superuser;
    {
        let roles = context.roles.role_definitions();
        roles::require_role_exists(&roles, &new_owner)?;
        let memberships = context.roles.role_memberships();
        let current_user = context.session.current_user_name();
        current_user_is_superuser = roles
            .get(&current_user)
            .is_some_and(|role| role.has(RoleAttribute::Superuser));
        roles::require_set_role(&roles, &memberships, &current_user, &new_owner)?;
    }
    Ok((new_owner, current_user_is_superuser))
}
