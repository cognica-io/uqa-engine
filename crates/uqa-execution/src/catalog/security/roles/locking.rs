//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind a role incarnation before waiting on its shared-object lock.

use crate::row_locks::{
    shared_objects::{SharedCatalogLock, SharedObjectLockSession},
    RelationLockMode,
};
use uqa_sql::{catalog::roles::guards::RoleCatalogGuards, SQLError};

pub const ROLE_CATALOG_CLASS_ID: u32 = 1260;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleBinding {
    pub name: String,
    pub oid: u32,
    pub object_id: [u8; 16],
}

#[derive(Clone, Copy)]
pub struct RoleLockContext<'a> {
    pub roles: &'a dyn RoleCatalogGuards,
    pub session: &'a dyn SharedObjectLockSession,
}

impl RoleLockContext<'_> {
    pub fn bind(&self, name: &str) -> Result<RoleBinding, SQLError> {
        let roles = self.roles.role_definitions();
        let role = roles.get(name).ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("role \"{name}\" does not exist"),
        })?;
        Ok(RoleBinding {
            name: name.to_string(),
            oid: u32::try_from(role.oid)
                .map_err(|_| SQLError::Internal("invalid role OID".into()))?,
            object_id: role.object_id,
        })
    }

    /// Never bind the name again after waiting: a replacement role must not acquire the old role's dependencies.
    pub fn lock(&self, bound: &RoleBinding, mode: RelationLockMode) -> Result<(), SQLError> {
        let guard = self.session.acquire_shared_catalog(
            SharedCatalogLock::Object {
                class_id: ROLE_CATALOG_CLASS_ID,
                oid: bound.oid,
            },
            mode,
        )?;
        self.session.refresh_shared_catalog()?;
        self.revalidate(bound)?;
        guard.retain();
        Ok(())
    }

    pub fn revalidate(&self, bound: &RoleBinding) -> Result<(), SQLError> {
        if self
            .roles
            .role_definitions()
            .get(&bound.name)
            .is_some_and(|role| {
                role.oid == i64::from(bound.oid) && role.object_id == bound.object_id
            })
        {
            Ok(())
        } else {
            Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role {} was concurrently dropped", bound.oid),
            })
        }
    }
}
