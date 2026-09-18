//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared relation rename publication and role-transfer authorization boundaries.
use crate::catalog::security::roles::{
    locking::{RoleBinding, RoleLockContext},
    RoleCatalogGuards,
};
use crate::row_locks::shared_objects::SharedObjectLockSession;
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::roles::{self, RoleReferenceNames},
    catalog::security::ownership::RelationOwnerSchemas,
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
    pub schemas: &'a dyn RelationOwnerSchemas,
    pub locks: &'a dyn SharedObjectLockSession,
}

impl<'a> RoleTransferContext<'a> {
    pub fn lock_context(&self) -> RoleLockContext<'a> {
        RoleLockContext {
            roles: self.roles,
            session: self.locks,
        }
    }

    pub fn bind(&self, requested: &str) -> Result<RoleBinding, SQLError> {
        self.lock_context()
            .bind(&roles::resolve_role_reference(self.session, requested))
    }
}
