//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! System relation ACL defaults, persistence validation and effective privilege rules.

use super::{
    table::{
        role_has_privilege, validate_table_security_invariants, TableAclPrivilege,
        TablePrivilegeCheck,
    },
    BoundTableSecurity, TableSecurity,
};
use crate::catalog::roles::identity::RoleSubject;
use crate::{
    ast::RoleAttribute,
    catalog::{
        roles::{RoleDefinition, RoleMembership, RoleMembershipKey},
        SystemRelation,
    },
};
use std::{collections::BTreeMap, ops::Deref};
use uqa_core::RelationIdentity;

/// One catalog tuple ACL and its replacement identity. Equal ACL values can still be different committed tuples.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SystemAcl {
    pub revision: [u8; 16],
    pub acl: Vec<super::table_binding::BoundTableAclEntry>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemRelationSecurity {
    pub table: Option<SystemAcl>,
    pub columns: BTreeMap<String, SystemAcl>,
}
impl SystemRelationSecurity {
    pub fn security(&self, relation: SystemRelation) -> BoundTableSecurity {
        let mut security = relation.bootstrap_security();
        if let Some(table) = &self.table {
            security.acl = Some(table.acl.clone());
        }
        security.column_acls = self
            .columns
            .iter()
            .filter(|(_, value)| !value.acl.is_empty())
            .map(|(name, value)| (name.clone(), value.acl.clone()))
            .collect();
        security
    }
    pub fn entry(&self, column: Option<&str>) -> Option<&SystemAcl> {
        column.map_or_else(|| self.table.as_ref(), |column| self.columns.get(column))
    }
}
pub type SystemRelationSecurities = BTreeMap<RelationIdentity, SystemRelationSecurity>;
pub type SystemRelationSecurityRead<'a> = Box<dyn Deref<Target = SystemRelationSecurities> + 'a>;

pub trait SystemRelationSecurityCatalog {
    fn system_relation_securities(&self) -> SystemRelationSecurityRead<'_>;
    fn system_relation_security(&self, relation: SystemRelation) -> BoundTableSecurity {
        security(&self.system_relation_securities(), relation)
    }
}

pub fn security(
    securities: &SystemRelationSecurities,
    relation: SystemRelation,
) -> BoundTableSecurity {
    securities
        .get(&RelationIdentity::new(
            relation.namespace(),
            relation.name(),
        ))
        .map(|entry| entry.security(relation))
        .unwrap_or_else(|| relation.bootstrap_security())
}

pub const METADATA_PREFIX: &str = "uqa.system_relation_security.v1:";

pub fn metadata_key(relation: SystemRelation, column: Option<&str>) -> String {
    // Built-in attribute names are immutable; length and quoting never depend on a search path.
    format!(
        "{METADATA_PREFIX}{}:{}",
        relation.qualified_name(),
        column.unwrap_or("")
    )
}

pub fn validate_security(
    relation: SystemRelation,
    security: &TableSecurity,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    if super::role_bindings::bind_role(roles, &security.role_owner, "system relation")?
        != crate::catalog::roles::RoleIdentity::BOOTSTRAP
    {
        return Err(format!(
            "system relation `{}` has an invalid owner",
            relation.qualified_name()
        ));
    }
    validate_table_security_invariants(security, Some(&relation.column_names()), roles)
}

fn masks_table_write(
    relation: SystemRelation,
    subject: &(impl RoleSubject + ?Sized),
    check: TablePrivilegeCheck,
    roles: &BTreeMap<String, RoleDefinition>,
) -> bool {
    relation.namespace() == "pg_catalog"
        && relation.kind() == "table"
        && !check.grant_option
        && matches!(
            check.privilege,
            TableAclPrivilege::Insert
                | TableAclPrivilege::Update
                | TableAclPrivilege::Delete
                | TableAclPrivilege::Truncate
        )
        && !subject
            .role_definition(roles)
            .is_some_and(|role| role.has(RoleAttribute::Superuser))
}

pub fn has_table_privilege(
    relation: SystemRelation,
    security: &TableSecurity,
    subject: &(impl RoleSubject + ?Sized),
    check: TablePrivilegeCheck,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> bool {
    !masks_table_write(relation, subject, check, roles)
        && role_has_privilege(security, subject, check, roles, memberships)
}

pub fn has_column_privilege(
    relation: SystemRelation,
    security: &TableSecurity,
    column: &str,
    subject: &(impl RoleSubject + ?Sized),
    check: TablePrivilegeCheck,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> bool {
    if !masks_table_write(relation, subject, check, roles) {
        return super::columns::role_has_column_privilege(
            security,
            column,
            subject,
            check,
            roles,
            memberships,
        );
    }
    // The system-table write mask applies to relation ACLs. Explicit attribute grants remain visible to column privilege inquiry.
    security.column_acls.get(column).is_some_and(|acl| {
        acl.iter().any(|entry| {
            entry.privileges.intersects(check.privilege.mask())
                && (entry.role == "PUBLIC"
                    || crate::catalog::roles::role_inherits(
                        roles,
                        memberships,
                        subject,
                        &entry.role,
                    ))
        })
    })
}

#[cfg(test)]
mod tests;
