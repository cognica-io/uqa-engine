//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role references of session-local relations are catalog dependencies after commit.

use super::context::TemporaryRoleDependencyCatalog;
use crate::{
    ast::RelationPersistence,
    catalog::{
        roles::RoleDefinition,
        security::{dependencies::AclRoleReferences, TableSecurity},
    },
    SQLError,
};
use std::collections::{BTreeMap, BTreeSet};

pub fn role_dependencies(
    catalog: &dyn TemporaryRoleDependencyCatalog,
    roles: &BTreeMap<String, RoleDefinition>,
    limit: usize,
) -> Result<BTreeSet<u32>, SQLError> {
    let mut referenced = BTreeSet::new();
    {
        let tables = catalog.tables();
        for (_, table) in tables.iter() {
            if table.persistence() == RelationPersistence::Temporary {
                table_dependencies(&table.security(), roles, limit, &mut referenced)?;
            }
        }
    }
    {
        let views = catalog.views();
        for view in views.values() {
            if view.persistence == RelationPersistence::Temporary {
                table_dependencies(&view.security(), roles, limit, &mut referenced)?;
            }
        }
    }
    {
        let sequences = catalog.sequences();
        let persistence = catalog.sequence_persistence();
        for (name, security) in sequences.iter() {
            if persistence.get(name) == Some(&RelationPersistence::Temporary) {
                add_role(&security.role_owner, roles, limit, &mut referenced)?;
                acl_dependencies(
                    security.acl.as_deref().unwrap_or_default(),
                    roles,
                    limit,
                    &mut referenced,
                )?;
            }
        }
    }
    Ok(referenced)
}

fn table_dependencies(
    security: &TableSecurity,
    roles: &BTreeMap<String, RoleDefinition>,
    limit: usize,
    referenced: &mut BTreeSet<u32>,
) -> Result<(), SQLError> {
    add_role(&security.role_owner, roles, limit, referenced)?;
    acl_dependencies(
        security.acl.as_deref().unwrap_or_default(),
        roles,
        limit,
        referenced,
    )?;
    for acl in security.column_acls.values() {
        acl_dependencies(acl, roles, limit, referenced)?;
    }
    Ok(())
}

fn acl_dependencies<T: AclRoleReferences>(
    acl: &[T],
    roles: &BTreeMap<String, RoleDefinition>,
    limit: usize,
    referenced: &mut BTreeSet<u32>,
) -> Result<(), SQLError> {
    for entry in acl {
        let (grantee, grantor) = entry.role_references();
        if let Some(grantee) = grantee {
            add_role(grantee, roles, limit, referenced)?;
        }
        if let Some(grantor) = grantor {
            add_role(grantor, roles, limit, referenced)?;
        }
    }
    Ok(())
}

fn add_role(
    name: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    limit: usize,
    referenced: &mut BTreeSet<u32>,
) -> Result<(), SQLError> {
    let role = roles.get(name).ok_or_else(|| SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("role \"{name}\" does not exist"),
    })?;
    if role.oid <= 0 {
        return Err(SQLError::Internal("invalid role OID".into()));
    }
    if role.oid != 10 {
        referenced.insert(
            u32::try_from(role.oid).map_err(|_| SQLError::Internal("invalid role OID".into()))?,
        );
        if referenced.len() > limit {
            return Err(SQLError::Routine {
                sqlstate: "53200".into(),
                message: "temporary role dependency capacity exhausted".into(),
            });
        }
    }
    Ok(())
}
