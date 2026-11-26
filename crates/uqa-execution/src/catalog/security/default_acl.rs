//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Change default ACL tuples in the same definition transaction as their shared and namespace dependencies.
use super::roles::locking::{RoleBinding, RoleLockContext};
use crate::row_locks::{shared_objects::SharedCatalogLock, RelationLockMode};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::catalog_role::RoleIdentity;
use uqa_sql::{
    ast::{AclRoleSpecification, AlterDefaultPrivilegesStmt},
    catalog::{
        dependencies::{DEFAULT_ACL_CLASS, NAMESPACE_CLASS},
        roles::{resolve_role_specification, role_inherits, RoleReference},
        security::{
            default_acl::{DefaultAclKey, DefaultAclRow, DefaultAcls},
            schema_inquiry::SchemaPrivilegeInquiry,
        },
    },
    SQLError,
};
use uqa_storage::CatalogFacade;

pub mod persistence;

pub trait DefaultAclPublication {
    fn default_acls(&self) -> DefaultAcls;
    fn default_acl_catalog(&self) -> Option<&dyn CatalogFacade>;
    fn prepare_default_acl_writer(&self) -> Result<(), SQLError>;
    fn publish_default_acls(&self, next: DefaultAcls);
}

pub struct DefaultAclContext<'a> {
    pub publication: &'a dyn DefaultAclPublication,
    pub roles: RoleLockContext<'a>,
    pub schemas: SchemaPrivilegeInquiry<'a>,
}
pub type DefaultAclWrite<'a> = Box<dyn FnOnce(&DefaultAclContext<'_>) -> Result<(), SQLError> + 'a>;
pub trait DefaultPrivilegeWrite {
    fn with_default_acl_write(&self, write: DefaultAclWrite<'_>) -> Result<(), SQLError>;
}

impl DefaultAclContext<'_> {
    fn grantees(
        &self,
        statement: &AlterDefaultPrivilegesStmt,
    ) -> Result<Vec<Option<RoleBinding>>, SQLError> {
        let definitions = self.roles.roles.role_definitions();
        statement
            .grantees
            .iter()
            .map(|spec| match spec {
                AclRoleSpecification::Public => Ok(None),
                AclRoleSpecification::Role(role) => {
                    resolve_role_specification(self.schemas.names, role)
                        .bind(&definitions)
                        .map(Some)
                }
            })
            .collect()
    }

    fn authorize(&self, owner: &RoleBinding) -> Result<(), SQLError> {
        let roles = self.roles.roles.role_definitions();
        owner.revalidate(&roles)?;
        if role_inherits(
            &roles,
            &self.roles.roles.role_memberships(),
            &self.schemas.names.current_role(),
            owner,
        ) {
            Ok(())
        } else {
            Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "permission denied to change default privileges".into(),
            })
        }
    }

    fn namespace(&self, name: &str) -> Result<u32, SQLError> {
        let security = self
            .schemas
            .schema_security_for_privilege(name)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{name}\" does not exist"),
            })?;
        u32::try_from(security.namespace_oid(name))
            .map_err(|_| SQLError::Internal("invalid namespace OID".into()))
    }

    pub fn alter(&self, statement: &AlterDefaultPrivilegesStmt) -> Result<(), SQLError> {
        let grantees = self.grantees(statement)?;
        let rights = statement.object.requested(&statement.privileges)?;
        let candidates: Vec<RoleReference> = if statement.owners.is_empty() {
            vec![self.schemas.names.current_role()]
        } else {
            statement
                .owners
                .iter()
                .map(|spec| resolve_role_specification(self.schemas.names, spec))
                .collect()
        };
        let mut owners = Vec::new();
        let mut namespaces = BTreeMap::new();
        let mut targets = Vec::new();
        for candidate in candidates {
            let owner = self.roles.bind(&candidate)?;
            self.authorize(&owner)?;
            let key = DefaultAclKey {
                owner: owner.identity(),
                namespace: 0,
                object: statement.object,
            };
            for name in &statement.schemas {
                let oid = self.namespace(name)?;
                let key = DefaultAclKey {
                    namespace: oid,
                    ..key
                };
                key.object.check_namespace(oid)?;
                namespaces.insert(oid, name);
                targets.push(key);
            }
            if statement.schemas.is_empty() {
                targets.push(key);
            }
            owners.push(owner);
        }
        let mut locks = BTreeMap::new();
        for role in grantees.iter().flatten() {
            locks.insert(role.oid, (role, RelationLockMode::AccessShare));
        }
        for owner in &owners {
            locks.insert(owner.oid, (owner, RelationLockMode::ShareRowExclusive));
        }
        for (role, mode) in locks.into_values() {
            self.roles.lock(role, mode)?;
        }
        for (&oid, &name) in &namespaces {
            let guard = self.roles.session.acquire_shared_catalog(
                SharedCatalogLock::Object {
                    class_id: NAMESPACE_CLASS,
                    oid,
                },
                RelationLockMode::AccessShare,
            )?;
            self.roles.session.refresh_shared_catalog()?;
            if self.namespace(name)? != oid {
                return Err(SQLError::Routine {
                    sqlstate: "40001".into(),
                    message: "schema was concurrently replaced".into(),
                });
            }
            guard.retain();
        }
        self.publication.prepare_default_acl_writer()?;
        for owner in &owners {
            self.authorize(owner)?;
        }
        let grantees: Vec<_> = grantees
            .iter()
            .map(|role| role.as_ref().map(RoleBinding::identity))
            .collect();
        let mut next = self.publication.default_acls();
        let mut changed = BTreeSet::new();
        for key in targets {
            let old = next.get(&key);
            let acl = key.change(old, statement, rights, &grantees)?;
            if old.map(|row| &row.acl) == acl.as_ref() {
                continue;
            }
            if let Some(acl) = acl {
                let oid = if let Some(row) = old {
                    row.oid
                } else {
                    let oid = crate::catalog::identity::reserve_new_catalog_oid(
                        self.roles.session,
                        DEFAULT_ACL_CLASS,
                        "default ACL",
                        |oid| Ok(next.values().any(|row| i64::from(row.oid) == oid)),
                    )?;
                    u32::try_from(oid)
                        .map_err(|_| SQLError::Internal("invalid default ACL OID".into()))?
                };
                let temporary =
                    self.schemas
                        .catalog
                        .temporary_namespace_oids()
                        .is_some_and(|oids| {
                            key.namespace == oids.namespace || key.namespace == oids.toast_namespace
                        });
                next.insert(
                    key,
                    DefaultAclRow {
                        oid,
                        temporary,
                        acl,
                    },
                )?;
            } else {
                next.remove(&key)?;
            }
            changed.insert(key);
        }
        publish(self.publication, next, changed)
    }
}

fn publish(
    publication: &dyn DefaultAclPublication,
    next: DefaultAcls,
    keys: BTreeSet<DefaultAclKey>,
) -> Result<(), SQLError> {
    if keys.is_empty() {
        return Ok(());
    }
    if let Some(catalog) = publication.default_acl_catalog() {
        for key in keys {
            persistence::write(catalog, key, next.get(&key))?;
        }
    }
    publication.publish_default_acls(next);
    Ok(())
}

pub fn remove(publication: &dyn DefaultAclPublication, key: DefaultAclKey) -> Result<(), SQLError> {
    let mut next = publication.default_acls();
    if next.remove(&key)? {
        publish(publication, next, BTreeSet::from([key]))?;
    }
    Ok(())
}

pub fn remove_grantees(
    publication: &dyn DefaultAclPublication,
    key: DefaultAclKey,
    roles: &BTreeSet<RoleIdentity>,
) -> Result<(), SQLError> {
    let mut next = publication.default_acls();
    let Some(row) = next.get(&key) else {
        return Ok(());
    };
    let Some(acl) = key.after_drop(row.acl.clone(), roles) else {
        return remove(publication, key);
    };
    if acl == row.acl {
        return Ok(());
    }
    next.insert(key, DefaultAclRow { acl, ..row.clone() })?;
    publish(publication, next, BTreeSet::from([key]))
}
