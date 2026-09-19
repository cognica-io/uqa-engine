//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table and column ACLs retain role incarnations independently of display names.

use super::{role_bindings, TableAclEntry, TablePrivileges, TableSecurity};
use crate::catalog::roles::{identity::RoleBinding, RoleDefinition, RoleIdentity, RoleReference};
use std::collections::BTreeMap;
use uqa_core::catalog_acl::AclGrantee;
use uqa_core::catalog_role::BoundAclEntry;

pub type BoundTableAclEntry = BoundAclEntry<TablePrivileges>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundTableSecurity {
    pub role_owner: RoleIdentity,
    pub acl: Option<Vec<BoundTableAclEntry>>,
    pub column_acls: BTreeMap<String, Vec<BoundTableAclEntry>>,
    pub acl_revisions: uqa_core::catalog_acl::RelationAclRevisions,
}

impl BoundTableSecurity {
    pub fn remove_column_acl(&mut self, column: &str) {
        self.column_acls.remove(column);
        self.acl_revisions.columns.remove(column);
    }

    pub fn rename_column_acl(&mut self, from: &str, to: &str) {
        if let Some(acl) = self.column_acls.remove(from) {
            self.column_acls.insert(to.to_owned(), acl);
        }
        if let Some(revision) = self.acl_revisions.columns.remove(from) {
            self.acl_revisions.columns.insert(to.to_owned(), revision);
        }
    }

    pub fn from_row(row: uqa_core::catalog_acl::BoundRelationSecurity) -> Self {
        Self {
            role_owner: row.role_owner,
            acl: row.acl,
            column_acls: row.column_acls,
            acl_revisions: row.acl_revisions,
        }
    }

    pub fn row(&self) -> uqa_core::catalog_acl::BoundRelationSecurity {
        uqa_core::catalog_acl::BoundRelationSecurity {
            role_owner: self.role_owner,
            acl: self.acl.clone(),
            column_acls: self.column_acls.clone(),
            acl_revisions: self.acl_revisions.clone(),
        }
    }

    pub fn owner(role_owner: RoleIdentity) -> Self {
        Self {
            role_owner,
            acl: None,
            column_acls: BTreeMap::new(),
            acl_revisions: uqa_core::catalog_acl::RelationAclRevisions::default(),
        }
    }

    pub fn owner_reference(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<RoleReference, crate::SQLError> {
        let name = role_bindings::role_name(roles, self.role_owner, "table")
            .map_err(crate::SQLError::Internal)?;
        Ok(RoleReference::Bound(std::sync::Arc::new(
            RoleBinding::from_definition(&roles[name])?,
        )))
    }

    pub fn bind(
        security: &TableSecurity,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<Self, String> {
        let bind = |name: &str| role_bindings::bind_role(roles, name, "table");
        let entries = |entries: &[TableAclEntry]| {
            entries
                .iter()
                .map(|entry| {
                    Ok(BoundTableAclEntry {
                        role: entry.role.role_name().map(bind).transpose()?,
                        grantor: bind(entry.grantor.as_deref().unwrap_or(&security.role_owner))?,
                        privileges: entry.privileges,
                        grant_options: entry.grant_options,
                    })
                })
                .collect::<Result<Vec<_>, String>>()
        };
        Ok(Self {
            role_owner: bind(&security.role_owner)?,
            acl_revisions: uqa_core::catalog_acl::RelationAclRevisions::default(),
            acl: security.acl.as_deref().map(entries).transpose()?,
            column_acls: security
                .column_acls
                .iter()
                .map(|(column, acl)| Ok((column.clone(), entries(acl)?)))
                .collect::<Result<_, String>>()?,
        })
    }

    /// Resolve names only from the role view accompanying this security snapshot.
    pub fn resolve(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<TableSecurity, String> {
        let name = |identity| role_bindings::role_name(roles, identity, "table").map(str::to_owned);
        let entries = |entries: &[BoundTableAclEntry]| {
            entries
                .iter()
                .map(|entry| {
                    Ok(TableAclEntry {
                        role: entry
                            .role
                            .map(&name)
                            .transpose()?
                            .map_or(AclGrantee::Public, AclGrantee::Role),
                        grantor: Some(name(entry.grantor)?),
                        privileges: entry.privileges,
                        grant_options: entry.grant_options,
                    })
                })
                .collect::<Result<Vec<_>, String>>()
        };
        Ok(TableSecurity {
            role_owner: name(self.role_owner)?,
            acl: self.acl.as_deref().map(entries).transpose()?,
            column_acls: self
                .column_acls
                .iter()
                .map(|(column, acl)| Ok((column.clone(), entries(acl)?)))
                .collect::<Result<_, String>>()?,
        })
    }

    pub fn validate(
        &self,
        columns: Option<&[String]>,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<(), String> {
        if self.acl_revisions.relation == Some([0; 16])
            || self.acl_revisions.columns.iter().any(|(column, revision)| {
                *revision == [0; 16] || columns.is_some_and(|columns| !columns.contains(column))
            })
        {
            return Err("invalid relation or attribute ACL tuple identity".into());
        }
        super::table::validate_table_security_invariants(&self.resolve(roles)?, columns, roles)
    }

    pub fn depends_on(&self, role: RoleIdentity) -> bool {
        self.role_owner == role
            || self
                .acl
                .iter()
                .flatten()
                .chain(self.column_acls.values().flatten())
                .any(|entry| entry.role == Some(role) || entry.grantor == role)
    }
}

#[cfg(test)]
mod tests;
