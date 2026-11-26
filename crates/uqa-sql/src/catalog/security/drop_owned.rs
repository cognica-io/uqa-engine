//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove a role as both grantee and grantor, cascading through lost grant options while retaining independently granted privileges.

use super::{
    columns, database, dependencies::AclRoleReferences, object_acl, schema, sequence, table,
    AclGrantee, SchemaSecurity, SequenceSecurity, TableSecurity,
};
use crate::{ast::ObjectAclEntry, SQLError};
use std::collections::BTreeSet;
use uqa_core::catalog_role::RoleIdentity;

fn grants<T: AclRoleReferences>(
    acl: Option<&[T]>,
    owner: &str,
    roles: &BTreeSet<String>,
) -> Vec<(AclGrantee, String)> {
    acl.into_iter()
        .flatten()
        .filter_map(|entry| {
            let (recipient, grantor) = entry.role_references();
            let grantor = grantor.unwrap_or(owner);
            if roles.contains(grantor) || recipient.is_some_and(|role| roles.contains(role)) {
                Some((
                    recipient.map_or(AclGrantee::Public, |name| AclGrantee::Role(name.to_owned())),
                    grantor.to_owned(),
                ))
            } else {
                None
            }
        })
        .collect()
}

pub struct RelationGrants<'a> {
    pub definitions: &'a crate::catalog::roles::RoleDefinitions,
    pub targets: &'a BTreeSet<String>,
}

impl super::table_grants::RelationAclChange for RelationGrants<'_> {
    fn roles(&self) -> &crate::catalog::roles::RoleDefinitions {
        self.definitions
    }
    fn requested(&self) -> &table::RequestedTablePrivileges {
        static EMPTY: table::RequestedTablePrivileges = table::RequestedTablePrivileges {
            table: Vec::new(),
            columns: Vec::new(),
        };
        &EMPTY
    }
    fn apply_to(
        &self,
        _: &super::table_grants::ResolvedTableGrantTarget,
        current: &TableSecurity,
    ) -> Result<(TableSecurity, usize), SQLError> {
        Ok((relation(current, self.targets)?, 0))
    }
    fn replaced_tuples(
        &self,
        before: &TableSecurity,
        after: &TableSecurity,
    ) -> Vec<Option<String>> {
        let mut changed = Vec::new();
        if before.acl != after.acl {
            changed.push(None);
        }
        changed.extend(
            before
                .column_acls
                .iter()
                .filter(|(name, acl)| after.column_acls.get(*name) != Some(*acl))
                .map(|(name, _)| Some(name.clone())),
        );
        changed
    }
}

pub fn relation(
    current: &TableSecurity,
    roles: &BTreeSet<String>,
) -> Result<TableSecurity, SQLError> {
    let mut next = current.clone();
    for (recipient, grantor) in grants(current.acl.as_deref(), &current.role_owner, roles) {
        for privilege in table::TableAclPrivilege::ALL {
            table::revoke_acl(
                &mut next,
                privilege,
                std::slice::from_ref(&recipient),
                &grantor,
                false,
                true,
            )?;
        }
    }
    for (column, acl) in &current.column_acls {
        for privilege in table::TableAclPrivilege::COLUMN_ALL {
            let before = columns::column_grant_option_roles(current, column, privilege);
            columns::revoke_dependent_column_acl(&mut next, column, privilege, &before, true)?;
        }
        for (recipient, grantor) in grants(Some(acl), &current.role_owner, roles) {
            for privilege in table::TableAclPrivilege::COLUMN_ALL {
                columns::revoke_column_acl(
                    &mut next,
                    column,
                    privilege,
                    std::slice::from_ref(&recipient),
                    &grantor,
                    false,
                    true,
                )?;
            }
        }
    }
    next.column_acls.retain(|_, acl| !acl.is_empty());
    Ok(next)
}

pub fn schema(
    current: &SchemaSecurity,
    roles: &BTreeSet<String>,
) -> Result<SchemaSecurity, SQLError> {
    use schema::SchemaAclPrivilege as Privilege;
    let mut next = current.clone();
    for (recipient, grantor) in grants(current.acl.as_deref(), &current.role_owner, roles) {
        for privilege in [Privilege::Usage, Privilege::Create] {
            schema::revoke_acl(
                &mut next,
                privilege,
                std::slice::from_ref(&recipient),
                &grantor,
                false,
                true,
            )?;
        }
    }
    Ok(next)
}

pub fn sequence(
    current: &SequenceSecurity,
    roles: &BTreeSet<String>,
) -> Result<SequenceSecurity, SQLError> {
    use sequence::AclPrivilege as Privilege;
    let mut next = current.clone();
    for (recipient, grantor) in grants(current.acl.as_deref(), &current.role_owner, roles) {
        for privilege in [Privilege::Usage, Privilege::Select, Privilege::Update] {
            sequence::revoke_acl(
                &mut next,
                privilege,
                std::slice::from_ref(&recipient),
                &grantor,
                false,
                true,
            )?;
        }
    }
    Ok(next)
}

pub fn database(
    current: &database::DatabaseSecurity,
    roles: &BTreeSet<String>,
) -> Result<database::DatabaseSecurity, SQLError> {
    use database::DatabaseAclPrivilege as Privilege;
    let mut next = current.clone();
    for (recipient, grantor) in grants(current.acl.as_deref(), &current.role_owner, roles) {
        for privilege in [Privilege::Connect, Privilege::Create, Privilege::Temporary] {
            database::revoke_acl(
                &mut next,
                privilege,
                std::slice::from_ref(&recipient),
                &grantor,
                false,
                true,
            )?;
        }
    }
    Ok(next)
}

pub fn object(
    owner: RoleIdentity,
    acl: &mut Option<Vec<ObjectAclEntry>>,
    roles: &BTreeSet<RoleIdentity>,
) -> Result<(), SQLError> {
    let grants: Vec<_> = acl
        .iter()
        .flatten()
        .filter(|entry| {
            roles.contains(&entry.grantor) || entry.role.is_some_and(|role| roles.contains(&role))
        })
        .map(|entry| (entry.role, entry.grantor))
        .collect();
    for (recipient, grantor) in grants {
        object_acl::revoke(owner, acl, recipient, grantor, false, true)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
