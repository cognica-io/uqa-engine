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
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::catalog::roles::RoleDefinition;
use uqa_sql::{catalog::roles::guards::RoleCatalogGuards, SQLError};

pub const ROLE_CATALOG_CLASS_ID: u32 = 1260;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleBinding {
    pub name: String,
    pub oid: u32,
    pub object_id: [u8; 16],
}

impl RoleBinding {
    pub fn from_definition(role: &RoleDefinition) -> Result<Self, SQLError> {
        if role.object_id == [0; 16] {
            return Err(SQLError::Internal("role has no object identity".into()));
        }
        Ok(Self {
            name: role.name.clone(),
            oid: u32::try_from(role.oid)
                .map_err(|_| SQLError::Internal("invalid role OID".into()))?,
            object_id: role.object_id,
        })
    }
}

#[derive(Default)]
pub struct RoleDependencyLocks {
    bindings: BTreeMap<String, RoleBinding>,
}

impl RoleDependencyLocks {
    pub fn missing(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
        dependencies: &BTreeSet<String>,
    ) -> Result<Vec<RoleBinding>, SQLError> {
        let mut pending = Vec::new();
        for name in dependencies {
            let role = roles.get(name).ok_or_else(|| SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{name}\" does not exist"),
            })?;
            // The bootstrap role is pinned and has no shared dependency entry.
            if role.oid == 10 {
                continue;
            }
            let bound = RoleBinding::from_definition(role)?;
            if let Some(original) = self.bindings.get(name) {
                if original != &bound {
                    return Err(concurrently_dropped(original.oid));
                }
            } else {
                pending.push(bound);
            }
        }
        Ok(pending)
    }

    pub fn acquire(
        &mut self,
        context: &RoleLockContext<'_>,
        pending: Vec<RoleBinding>,
    ) -> Result<(), SQLError> {
        for bound in pending {
            context.lock(&bound, RelationLockMode::AccessShare)?;
            self.bindings.insert(bound.name.clone(), bound);
        }
        Ok(())
    }
}

fn concurrently_dropped(oid: u32) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("role {oid} was concurrently dropped"),
    }
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
        RoleBinding::from_definition(role)
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
            Err(concurrently_dropped(bound.oid))
        }
    }
}
